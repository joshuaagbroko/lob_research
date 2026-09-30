//! Trace one event: print top-of-book before/after and the apply result.
//!
//! Usage:
//!   cargo run -p lob-cli --example trace -- \
//!     <message.csv> <orderbook.csv> <event_index>

use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader};

use lob_core::{BookError, Event, OrderBook, Side};
use lob_cli::lobster::{parse_message, parse_snapshot_row, Msg, EMPTY_ASK, EMPTY_BID};

fn top(book: &OrderBook) -> String {
    let bb = book.best_bid().map(|(p, s)| format!("{p}x{s}")).unwrap_or("-".into());
    let ba = book.best_ask().map(|(p, s)| format!("{p}x{s}")).unwrap_or("-".into());
    format!("bid {bb} | ask {ba}")
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: trace <message.csv> <orderbook.csv> <event_index>");
        std::process::exit(1);
    }
    let target: usize = args[3].parse().expect("event_index must be an integer");

    // --- seed from row 0 (same as main.rs) ---
    let mut ref_lines = BufReader::new(File::open(&args[2]).unwrap()).lines();
    let first = ref_lines.next().unwrap().unwrap();
    let row = parse_snapshot_row(&first).unwrap();
    let mut book = OrderBook::new();
    let mut ghosts: HashMap<(bool, i64), u64> = HashMap::new();
    let mut next_ghost = u64::MAX;
    for lvl in row.chunks(4) {
        let (ap, asz, bp, bsz) = (lvl[0], lvl[1], lvl[2], lvl[3]);
        for (is_bid, price, size) in [(false, ap, asz), (true, bp, bsz)] {
            if size == 0 || price == EMPTY_ASK || price == EMPTY_BID { continue; }
            let side = if is_bid { Side::Bid } else { Side::Ask };
            book.apply(Event::Add { order_id: next_ghost, side, price, size: size as u32 }).unwrap();
            ghosts.insert((is_bid, price), next_ghost);
            next_ghost -= 1;
        }
    }

    // --- replay, stop at target ---
    let mut lines = BufReader::new(File::open(&args[1]).unwrap()).lines();
    for i in 0..=target {
        let line = match lines.next() {
            Some(Ok(l)) => l,
            Some(Err(e)) => { eprintln!("read error at row {i}: {e}"); return; }
            None => { eprintln!("only {i} rows in file"); return; }
        };
        if line.trim().is_empty() { continue; }
        let m: Msg = match parse_message(&line) {
            Ok(m) => m,
            Err(e) => { eprintln!("parse error at row {i}: {e} | {line}"); return; }
        };

        if i < target { 
            // silently advance; no tracing
            let _ = apply_event(&mut book, &ghosts, &m);
            continue;
        }

        // --- this is the event we care about ---
        eprintln!("=== event {i} ===");
        eprintln!("raw: {line}");
        eprintln!("parsed: kind={} id={} side={:?} px={} sz={}",
                  m.kind, m.id, m.side, m.price, m.size);
        eprintln!("before: {}", top(&book));

        let before_unknown = book.best_bid();
        let result = apply_event(&mut book, &ghosts, &m);
        eprintln!("apply: {result:?}");
        eprintln!("after:  {}", top(&book));
        let _ = before_unknown;
    }
}

fn apply_event(book: &mut OrderBook, ghosts: &HashMap<(bool, i64), u64>, m: &Msg) -> Result<(), String> {
    let ev = match m.kind {
        1 => Event::Add { order_id: m.id, side: m.side, price: m.price, size: m.size },
        2 => Event::Cancel { order_id: m.id, size: m.size },
        3 => Event::Delete { order_id: m.id },
        4 => Event::Execute { order_id: m.id, size: m.size },
        _ => return Ok(()),
    };
    match book.apply(ev) {
        Ok(()) => Ok(()),
        Err(BookError::UnknownOrder(_)) => match ghosts.get(&(m.side == Side::Bid, m.price)) {
            Some(&g) => book.apply(Event::Cancel { order_id: g, size: m.size })
                .map_err(|e| format!("ghost reduce: {e:?}")),
            None => Err("unknown order, no ghost".into()),
        },
        Err(e) => Err(format!("{e:?}")),
    }
}