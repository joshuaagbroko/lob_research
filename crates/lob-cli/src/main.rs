//! usage: lob-cli <message.csv> <orderbook.csv> <out.csv>
//!
//! Replays LOBSTER messages through lob-core and writes the reconstructed book
//! in LOBSTER's orderbook layout, one row per message.
//!
//! ## Seeding
//!
//! LOBSTER starts the day with a populated book we never see built. Row 0 of
//! the reference orderbook is the state AFTER message 0, so we seed one ghost
//! order per visible level from that row and skip replaying message 0.
//!
//! ## Ghost ledger
//!
//! A level-10 file only shows orders inside the top 10 price levels. Multiple
//! pre-existing orders can rest at the same invisible price, so a single ghost
//! per price is insufficient: one can drain to zero while others at that price
//! still need resolving, or a later event can need more size than the current
//! ghost holds. So each (side, price) key maps to a FIFO queue of ghost
//! `(id, remaining size)` pairs:
//!
//!   - An event needing `n` shares drains the oldest ghost first.
//!   - A fully drained ghost is popped from the queue (lob-core removes
//!     zero-size orders from the book itself).
//!   - If the queue is empty or a single ghost is too small, a new ghost is
//!     created: sized from the reference row for the PRECEDING message
//!     (`reconciled_exact`), or the remaining need if the price isn't visible
//!     in the reference row (`reconciled_fallback`).
//!
//! Genuine anomalies -- unknown order, oversize, duplicate -- on a REAL
//! tracked order are counted separately and never mixed with ghost accounting.

mod lobster;

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process;

use lob_cli::lobster::{
    format_snapshot, parse_message, parse_snapshot_row, Msg, EMPTY_ASK, EMPTY_BID,
};
use lob_core::{BookError, Event, OrderBook, Side};

const MAX_LOGGED: u64 = 200;

type Ledger = HashMap<(bool, i64), VecDeque<(u64, u32)>>;

#[derive(Default)]
struct Stats {
    by_type: [u64; 8],
    ghost_hits: u64,
    reconciled_exact: u64,
    reconciled_fallback: u64,
    unknown: u64,
    duplicate: u64,
    oversize: u64,
}

impl Stats {
    fn record_real(&mut self, e: &BookError) {
        match e {
            BookError::UnknownOrder(_) => self.unknown += 1,
            BookError::DuplicateOrder(_) => self.duplicate += 1,
            BookError::Oversize(_) => self.oversize += 1,
        }
    }
}

fn die(msg: String) -> ! {
    eprintln!("error: {msg}");
    process::exit(1);
}

fn side_str(s: Side) -> &'static str {
    match s {
        Side::Bid => "bid",
        Side::Ask => "ask",
    }
}

/// Aggregate size at `price` on `side` within a parsed orderbook row, or None
/// if that price doesn't appear in any of the row's visible levels.
fn aggregate_at(row: &[i64], levels: usize, side: Side, price: i64) -> Option<i64> {
    for lvl in 0..levels {
        let base = lvl * 4;
        let (ap, asz, bp, bsz) = (row[base], row[base + 1], row[base + 2], row[base + 3]);
        match side {
            Side::Ask if ap == price => return Some(asz),
            Side::Bid if bp == price => return Some(bsz),
            _ => {}
        }
    }
    None
}

/// Seeds one ghost order at `(side, price)` and appends it to the ledger.
fn seed_ghost(
    book: &mut OrderBook,
    ledger: &mut Ledger,
    next_ghost: &mut u64,
    side: Side,
    price: i64,
    size: u32,
) {
    let gid = *next_ghost;
    *next_ghost -= 1;
    book.apply(Event::Add { order_id: gid, side, price, size })
        .expect("ghost ids are unique and sizes positive");
    ledger
        .entry((side == Side::Bid, price))
        .or_default()
        .push_back((gid, size));
}

/// Satisfies `need` shares of an unresolved event by draining the ghost ledger
/// at `(side, price)`, seeding new ghosts as required. Always fully succeeds.
#[allow(clippy::too_many_arguments)]
fn consume_ghost(
    book: &mut OrderBook,
    ledger: &mut Ledger,
    next_ghost: &mut u64,
    side: Side,
    price: i64,
    mut need: u32,
    prev_row: Option<&[i64]>,
    levels: usize,
    st: &mut Stats,
) {
    let key = (side == Side::Bid, price);

    while need > 0 {
        let front = ledger.get_mut(&key).and_then(|dq| dq.front().copied());
        match front {
            Some((gid, gsz)) if gsz <= need => {
                book.apply(Event::Cancel { order_id: gid, size: gsz })
                    .expect("ledger stays in sync with book");
                need -= gsz;
                ledger.get_mut(&key).unwrap().pop_front();
                st.ghost_hits += 1;
            }
            Some((gid, _gsz)) => {
                // Front ghost is larger than the need: partially reduce it.
                book.apply(Event::Cancel { order_id: gid, size: need })
                    .expect("ledger stays in sync with book");
                if let Some(dq) = ledger.get_mut(&key) {
                    if let Some(back) = dq.front_mut() {
                        back.1 -= need;
                    }
                }
                st.ghost_hits += 1;
                need = 0;
            }
            None => {
                // Ledger empty at this price. Seed a new ghost.
                let ref_agg = prev_row.and_then(|row| aggregate_at(row, levels, side, price));
                let size = match ref_agg {
                    Some(agg) if agg > 0 => {
                        st.reconciled_exact += 1;
                        agg as u32
                    }
                    _ => {
                        st.reconciled_fallback += 1;
                        need
                    }
                };
                seed_ghost(book, ledger, next_ghost, side, price, size);
            }
        }
    }

    if ledger.get(&key).is_some_and(VecDeque::is_empty) {
        ledger.remove(&key);
    }
}

/// Handles one message against real tracked orders, falling back to ghost
/// reconciliation for anything referencing an order we never saw added.
#[allow(clippy::too_many_arguments)]
fn handle(
    book: &mut OrderBook,
    ledger: &mut Ledger,
    next_ghost: &mut u64,
    m: &Msg,
    row_idx: usize,
    prev_row: Option<&[i64]>,
    levels: usize,
    st: &mut Stats,
    anomalies_logged: &mut u64,
) {
    let ev = match m.kind {
        1 => Event::Add { order_id: m.id, side: m.side, price: m.price, size: m.size },
        2 => Event::Cancel { order_id: m.id, size: m.size },
        3 => Event::Delete { order_id: m.id },
        4 => Event::Execute { order_id: m.id, size: m.size },
        _ => return,
    };

    match book.apply(ev) {
        Ok(()) => {}
        Err(BookError::UnknownOrder(_)) => {
            consume_ghost(
                book, ledger, next_ghost, m.side, m.price, m.size, prev_row, levels, st,
            );
        }
        Err(e @ (BookError::Oversize(_) | BookError::DuplicateOrder(_))) => {
            st.record_real(&e);
            *anomalies_logged += 1;
            if *anomalies_logged <= MAX_LOGGED {
                eprintln!(
                    "ANOMALY row {row_idx}: type={} id={} side={} price={} size={} -> {e:?}",
                    m.kind, m.id, side_str(m.side), m.price, m.size
                );
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        die("usage: lob-cli <message.csv> <orderbook.csv> <out.csv>".into());
    }
    let open = |p: &str| BufReader::new(File::open(p).unwrap_or_else(|e| die(format!("{p}: {e}"))));

    // --- Seed from row 0 of the reference orderbook. ---
    let mut ref_lines = open(&args[2]).lines();
    let first = ref_lines
        .next()
        .unwrap_or_else(|| die("orderbook file is empty".into()))
        .unwrap_or_else(|e| die(e.to_string()));
    let seed_row = parse_snapshot_row(&first).unwrap_or_else(|e| die(format!("orderbook row 0: {e}")));
    let levels = seed_row.len() / 4;

    let mut book = OrderBook::new();
    let mut ledger: Ledger = HashMap::new();
    let mut next_ghost = u64::MAX;

    for lvl in 0..levels {
        let base = lvl * 4;
        let (ap, asz, bp, bsz) = (
            seed_row[base],
            seed_row[base + 1],
            seed_row[base + 2],
            seed_row[base + 3],
        );
        for (side, price, size) in [(Side::Ask, ap, asz), (Side::Bid, bp, bsz)] {
            if size == 0 || price == EMPTY_ASK || price == EMPTY_BID {
                continue;
            }
            seed_ghost(&mut book, &mut ledger, &mut next_ghost, side, price, size as u32);
        }
    }

    // `prev_row` is "reference state before the message about to be handled".
    // Message 1 is preceded by row 0, which we already have as seed_row.
    let mut prev_row: Option<Vec<i64>> = Some(seed_row);
    let mut ref_exhausted_warned = false;

    let mut out = BufWriter::new(
        File::create(&args[3]).unwrap_or_else(|e| die(format!("{}: {e}", args[3]))),
    );
    let mut st = Stats::default();
    let mut rows = 0u64;
    let mut anomalies_logged = 0u64;

    for (i, line) in open(&args[1]).lines().enumerate() {
        let line = line.unwrap_or_else(|e| die(e.to_string()));
        if line.trim().is_empty() {
            continue;
        }
        let m = parse_message(&line).unwrap_or_else(|e| die(format!("message row {i}: {e}")));
        st.by_type[(m.kind as usize).min(7)] += 1;

        if i > 0 {
            handle(
                &mut book,
                &mut ledger,
                &mut next_ghost,
                &m,
                i,
                prev_row.as_deref(),
                levels,
                &mut st,
                &mut anomalies_logged,
            );
        }

        if let (Some((bb, _)), Some((ba, _))) = (book.best_bid(), book.best_ask()) {
            if bb >= ba {
                eprintln!("CROSSED book at row {i}: bid {bb} >= ask {ba}");
            }
        }

        writeln!(out, "{}", format_snapshot(&book, levels))
            .unwrap_or_else(|e| die(e.to_string()));
        rows += 1;

        // Advance prev_row to "row i" so it becomes the PRECEDING row for message i+1.
        match ref_lines.next() {
            Some(Ok(next_line)) => match parse_snapshot_row(&next_line) {
                Ok(parsed) => prev_row = Some(parsed),
                Err(e) => die(format!("orderbook row {i}: {e}")),
            },
            Some(Err(e)) => die(e.to_string()),
            None => {
                if !ref_exhausted_warned {
                    ref_exhausted_warned = true;
                    eprintln!(
                        "WARN: reference orderbook exhausted before message file (at message row {i})"
                    );
                }
                prev_row = None;
            }
        }
    }
    out.flush().unwrap_or_else(|e| die(e.to_string()));

    let reconciled = st.reconciled_exact + st.reconciled_fallback;
    eprintln!("rows written: {rows} ({levels} levels)");
    eprintln!("messages by type (1..7): {:?}", &st.by_type[1..]);
    eprintln!(
        "ghost reconciliation: {} events drained ({} exact-sized from reference, {} fallback-sized)",
        st.ghost_hits, st.reconciled_exact, st.reconciled_fallback
    );
    if reconciled == 0 && st.ghost_hits == 0 {
        eprintln!("  (no ghost reconciliation needed -- clean run)");
    }
    eprintln!(
        "genuine anomalies on real tracked orders: unknown {}, duplicate {}, oversize {}",
        st.unknown, st.duplicate, st.oversize
    );
    if anomalies_logged > MAX_LOGGED {
        eprintln!("  ... {} more anomalies not logged (raise MAX_LOGGED to see them)", anomalies_logged - MAX_LOGGED);
    }
}