//! usage: lob-cli <message.csv> <orderbook.csv> <out.csv>
//! Replays LOBSTER messages through lob-core and writes the reconstructed book in
//! LOBSTER's orderbook layout, one row per message.
//!
//! Seeding: LOBSTER starts the day with a populated book we never see being built.
//! The book is seeded from row 0 of the reference orderbook file (state AFTER message 0),
//! one "ghost" order per visible level, and message 0 is not replayed. Events that hit an
//! unknown order id (a pre-existing order) are charged to that level's ghost. Row 0 is
//! therefore validated trivially, and queue position of pre-existing orders is unknowable.

mod lobster;

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process;

use lob_core::{BookError, Event, OrderBook, Side};
use lobster::{format_snapshot, parse_message, parse_snapshot_row, Msg, EMPTY_ASK, EMPTY_BID};

#[derive(Default)]
struct Stats {
    by_type: [u64; 8],
    ghost_hits: u64,
    unknown: u64,
    duplicate: u64,
    oversize: u64,
}

impl Stats {
    fn record(&mut self, e: BookError) {
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

fn handle(book: &mut OrderBook, ghosts: &HashMap<(bool, i64), u64>, m: &Msg, st: &mut Stats) {
    let ev = match m.kind {
        1 => Event::Add { order_id: m.id, side: m.side, price: m.price, size: m.size },
        2 => Event::Cancel { order_id: m.id, size: m.size },
        3 => Event::Delete { order_id: m.id },
        4 => Event::Execute { order_id: m.id, size: m.size },
        _ => return, // 5 hidden execution, 6 cross trade, 7 halt: visible book unchanged
    };
    match book.apply(ev) {
        Ok(()) => {}
        Err(BookError::UnknownOrder(_)) => match ghosts.get(&(m.side == Side::Bid, m.price)) {
            Some(&g) => match book.apply(Event::Cancel { order_id: g, size: m.size }) {
                Ok(()) => st.ghost_hits += 1,
                Err(e) => st.record(e),
            },
            None => st.unknown += 1,
        },
        Err(e) => st.record(e),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        die("usage: lob-cli <message.csv> <orderbook.csv> <out.csv>".into());
    }
    let open = |p: &str| BufReader::new(File::open(p).unwrap_or_else(|e| die(format!("{p}: {e}"))));

    // Seed from row 0 of the reference orderbook.
    let mut ref_lines = open(&args[2]).lines();
    let first = ref_lines
        .next()
        .unwrap_or_else(|| die("orderbook file is empty".into()))
        .unwrap_or_else(|e| die(e.to_string()));
    let row = parse_snapshot_row(&first).unwrap_or_else(|e| die(format!("orderbook row 0: {e}")));
    let levels = row.len() / 4;

    let mut book = OrderBook::new();
    let mut ghosts: HashMap<(bool, i64), u64> = HashMap::new();
    let mut next_ghost = u64::MAX;
    for lvl in row.chunks(4) {
        let (ap, asz, bp, bsz) = (lvl[0], lvl[1], lvl[2], lvl[3]);
        for (is_bid, price, size) in [(false, ap, asz), (true, bp, bsz)] {
            if size == 0 || price == EMPTY_ASK || price == EMPTY_BID {
                continue;
            }
            let side = if is_bid { Side::Bid } else { Side::Ask };
            book.apply(Event::Add { order_id: next_ghost, side, price, size: size as u32 })
                .expect("ghost ids are unique");
            ghosts.insert((is_bid, price), next_ghost);
            next_ghost -= 1;
        }
    }

    let mut out = BufWriter::new(File::create(&args[3]).unwrap_or_else(|e| die(format!("{}: {e}", args[3]))));
    let mut st = Stats::default();
    let mut rows = 0u64;
    for (i, line) in open(&args[1]).lines().enumerate() {
        let line = line.unwrap_or_else(|e| die(e.to_string()));
        if line.trim().is_empty() {
            continue;
        }
        let m = parse_message(&line).unwrap_or_else(|e| die(format!("message row {i}: {e}")));
        st.by_type[(m.kind as usize).min(7)] += 1;
        if i > 0 {
            handle(&mut book, &ghosts, &m, &mut st);
        }
        writeln!(out, "{}", format_snapshot(&book, levels)).unwrap_or_else(|e| die(e.to_string()));
        rows += 1;
    }
    out.flush().unwrap_or_else(|e| die(e.to_string()));

    eprintln!("rows written: {rows} ({levels} levels)");
    eprintln!("messages by type (1..7): {:?}", &st.by_type[1..]);
    eprintln!("events charged to pre-existing (ghost) orders: {}", st.ghost_hits);
    eprintln!("errors: unknown order {}, duplicate {}, oversize {}", st.unknown, st.duplicate, st.oversize);
}