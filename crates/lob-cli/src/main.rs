//! usage: lob-cli <message.csv> <orderbook.csv> <out.csv>
//! Replays LOBSTER messages through lob-core and writes the reconstructed book in
//! LOBSTER's orderbook layout, one row per message.
//!
//! Boundary reconciliation (phantom table): a level-10 file only shows orders inside
//! the top 10 price levels. Orders resting deeper than that at open -- or referenced by
//! an event before we ever saw them added -- are invisible to us. Rather than injecting
//! synthetic orders into lob-core's real OrderBook (which was tried first and produced
//! two bug classes: stale ids after a synthetic order drained to zero, and Oversize
//! errors when one synthetic order wasn't big enough), invisible inventory is tracked
//! as a plain scalar side table, `phantom: (side, price) -> remaining size`, kept
//! entirely separate from lob-core. lob-core's `apply()` is called ONLY for real,
//! visible events -- phantom bookkeeping is pure arithmetic that cannot error.
//!
//! Seeding: row 0 of the reference file is the state AFTER message 0, so phantom is
//! bootstrapped directly from it (every nonzero level becomes an initial phantom
//! balance) and message 0 is never replayed -- its own order is indistinguishable from
//! any other pre-existing invisible order at its price, which is accurate: if a later
//! message references message 0's order id, it will correctly draw down the same
//! phantom pool.
//!
//! Top-up: when an event needs more phantom size at a price than currently recorded,
//! the balance is reset (not added to) from the reference row for the PRECEDING
//! message -- never the current one, which would be circular (using an event's own
//! outcome to resolve itself). If that price isn't visible in the preceding reference
//! row either, the balance is set to exactly what this event needs (`reconciled_fallback`);
//! otherwise it's set from the reference's own total minus our known real size there
//! (`reconciled_exact`).
//!
//! Output: each row merges lob-core's real levels with the phantom table (real +
//! phantom per price, top N by price) -- lob-core never holds phantom liquidity itself.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process;

use lob_cli::lobster::{parse_message, parse_snapshot_row, Msg, EMPTY_ASK, EMPTY_BID};
use lob_core::{BookError, Event, OrderBook, Side};

/// (is_bid, price) -> remaining invisible size at that price.
type Phantom = HashMap<(bool, i64), i64>;

#[derive(Default)]
struct Stats {
    by_type: [u64; 8],
    phantom_hits: u64,
    reconciled_exact: u64,
    reconciled_fallback: u64,
    // Errors on REAL tracked orders only -- phantom accounting never produces these.
    unknown: u64,
    duplicate: u64,
    oversize: u64,
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

/// Aggregate size at `price` on `side` within a parsed orderbook row, or None if that
/// price doesn't appear in any of the row's visible levels.
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

/// Ensures `phantom[(side,price)] >= need`, topping up from the preceding reference
/// row if the current balance is insufficient. Pure arithmetic; cannot fail.
fn ensure_phantom(
    phantom: &mut Phantom,
    book: &OrderBook,
    side: Side,
    price: i64,
    need: u32,
    prev_row: Option<&[i64]>,
    levels: usize,
    st: &mut Stats,
) {
    let key = (side == Side::Bid, price);
    let avail = *phantom.get(&key).unwrap_or(&0);
    if avail >= need as i64 {
        return;
    }
    let known = book.size_at(side, price) as i64;
    let ref_agg = prev_row.and_then(|row| aggregate_at(row, levels, side, price));
    let target = match ref_agg {
        Some(agg) if agg - known > need as i64 => {
            st.reconciled_exact += 1;
            agg - known
        }
        _ => {
            // Either not visible in the preceding reference window, or the reference
            // total there doesn't cover this event either -- take exactly what's needed.
            st.reconciled_fallback += 1;
            need as i64
        }
    };
    phantom.insert(key, target);
}

/// Handles one message against real tracked orders, falling back to the phantom table
/// for anything referencing an order we never saw added.
fn handle(
    book: &mut OrderBook,
    phantom: &mut Phantom,
    prev_row: Option<&[i64]>,
    levels: usize,
    row: usize,
    m: &Msg,
    st: &mut Stats,
    anomalies_logged: &mut u64,
) {
    const MAX_LOGGED: u64 = 200;
    let ev = match m.kind {
        1 => Event::Add { order_id: m.id, side: m.side, price: m.price, size: m.size },
        2 => Event::Cancel { order_id: m.id, size: m.size },
        3 => Event::Delete { order_id: m.id },
        4 => Event::Execute { order_id: m.id, size: m.size },
        _ => return, // 5 hidden execution, 6 cross trade, 7 halt: visible book unchanged
    };
    match book.apply(ev) {
        Ok(()) => {}
        Err(BookError::UnknownOrder(_)) => {
            ensure_phantom(phantom, book, m.side, m.price, m.size, prev_row, levels, st);
            let key = (m.side == Side::Bid, m.price);
            let remaining = phantom.get_mut(&key).expect("just ensured");
            *remaining -= m.size as i64;
            st.phantom_hits += 1;
            if *remaining <= 0 {
                phantom.remove(&key);
            }
        }
        Err(e @ (BookError::Oversize(_) | BookError::DuplicateOrder(_))) => {
            match e {
                BookError::Oversize(_) => st.oversize += 1,
                BookError::DuplicateOrder(_) => st.duplicate += 1,
                BookError::UnknownOrder(_) => unreachable!(),
            }
            *anomalies_logged += 1;
            if *anomalies_logged <= MAX_LOGGED {
                eprintln!(
                    "ANOMALY row {row}: type={} id={} side={} price={} size={} -> {:?} (on a REAL tracked order, not phantom)",
                    m.kind, m.id, side_str(m.side), m.price, m.size, e
                );
            }
        }
    }
}

/// Top `n` combined (real + phantom) levels for one side, best price first.
fn combined_levels(book: &OrderBook, phantom: &Phantom, side: Side, n: usize) -> Vec<(i64, u32)> {
    use std::collections::BTreeSet;
    let is_bid = side == Side::Bid;
    // Candidate prices: enough real levels to be safe, plus every phantom-only price
    // on this side (a price can be ALL phantom, with no real order there at all).
    let mut prices: BTreeSet<i64> = book.levels(side, n.max(50)).into_iter().map(|(p, _)| p).collect();
    for &(pb, price) in phantom.keys() {
        if pb == is_bid {
            prices.insert(price);
        }
    }
    let mut rows: Vec<(i64, u32)> = prices
        .into_iter()
        .map(|p| {
            let real = book.size_at(side, p) as i64;
            let ph = phantom.get(&(is_bid, p)).copied().unwrap_or(0);
            (p, (real + ph).max(0) as u32)
        })
        .filter(|&(_, sz)| sz > 0)
        .collect();
    if is_bid {
        rows.sort_by(|a, b| b.0.cmp(&a.0));
    } else {
        rows.sort_by(|a, b| a.0.cmp(&b.0));
    }
    rows.truncate(n);
    rows
}

fn format_combined(book: &OrderBook, phantom: &Phantom, levels: usize) -> String {
    let asks = combined_levels(book, phantom, Side::Ask, levels);
    let bids = combined_levels(book, phantom, Side::Bid, levels);
    let mut cols = Vec::with_capacity(levels * 4);
    for i in 0..levels {
        let (ap, asz) = asks.get(i).copied().unwrap_or((EMPTY_ASK, 0));
        let (bp, bsz) = bids.get(i).copied().unwrap_or((EMPTY_BID, 0));
        cols.extend([ap, asz as i64, bp, bsz as i64]);
    }
    cols.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(",")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        die("usage: lob-cli <message.csv> <orderbook.csv> <out.csv>".into());
    }
    let open = |p: &str| BufReader::new(File::open(p).unwrap_or_else(|e| die(format!("{p}: {e}"))));

    let mut ref_lines = open(&args[2]).lines();
    let first = ref_lines
        .next()
        .unwrap_or_else(|| die("orderbook file is empty".into()))
        .unwrap_or_else(|e| die(e.to_string()));
    let seed_row = parse_snapshot_row(&first).unwrap_or_else(|e| die(format!("orderbook row 0: {e}")));
    let levels = seed_row.len() / 4;

    let mut book = OrderBook::new();
    let mut phantom: Phantom = HashMap::new();
    for lvl in seed_row.chunks(4) {
        let (ap, asz, bp, bsz) = (lvl[0], lvl[1], lvl[2], lvl[3]);
        if asz > 0 && ap != EMPTY_ASK {
            phantom.insert((false, ap), asz);
        }
        if bsz > 0 && bp != EMPTY_BID {
            phantom.insert((true, bp), bsz);
        }
    }
    // prev_row tracks "reference state before the message about to be handled". Message 1
    // is preceded by row 0, which we already have as seed_row.
    let mut prev_row: Option<Vec<i64>> = Some(seed_row);
    let mut ref_exhausted_warned = false;

    let mut out = BufWriter::new(File::create(&args[3]).unwrap_or_else(|e| die(format!("{}: {e}", args[3]))));
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
            handle(&mut book, &mut phantom, prev_row.as_deref(), levels, i, &m, &mut st, &mut anomalies_logged);
        }

        if let (Some((bb, _)), Some((ba, _))) = (book.best_bid(), book.best_ask()) {
            if bb >= ba {
                eprintln!("CROSSED real-only book at row {i}: bid {bb} >= ask {ba}");
            }
        }

        writeln!(out, "{}", format_combined(&book, &phantom, levels)).unwrap_or_else(|e| die(e.to_string()));
        rows += 1;

        // Advance prev_row to "row i" so it's ready as the PRECEDING row for message i+1.
        match ref_lines.next() {
            Some(Ok(next_line)) => match parse_snapshot_row(&next_line) {
                Ok(parsed) => prev_row = Some(parsed),
                Err(e) => die(format!("orderbook row {i}: {e}")),
            },
            Some(Err(e)) => die(e.to_string()),
            None => {
                // Expected on the very last message (no "row i" follows it).
                ref_exhausted_warned = true;
            }
        }
    }
    out.flush().unwrap_or_else(|e| die(e.to_string()));
    let _ = ref_exhausted_warned;

    if anomalies_logged > 200 {
        eprintln!("... {} more anomalies not logged (raise MAX_LOGGED in handle() to see them)", anomalies_logged - 200);
    }
    eprintln!("rows written: {rows} ({levels} levels)");
    eprintln!("messages by type (1..7): {:?}", &st.by_type[1..]);
    eprintln!(
        "phantom reconciliation: {} shares drawn down ({} newly set from reference, {} fallback-sized)",
        st.phantom_hits, st.reconciled_exact, st.reconciled_fallback
    );
    eprintln!(
        "genuine anomalies on real tracked orders: unknown {}, duplicate {}, oversize {}",
        st.unknown, st.duplicate, st.oversize
    );
}