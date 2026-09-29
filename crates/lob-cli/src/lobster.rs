//! LOBSTER file formats (no headers).
//! message:   time, type, order_id, size, price, direction
//! orderbook: per level i: ask_price_i, ask_size_i, bid_price_i, bid_size_i
//! Empty levels use sentinel prices and size 0.

use std::str::FromStr;

use lob_core::{OrderBook, Side};

pub const EMPTY_ASK: i64 = 9_999_999_999;
pub const EMPTY_BID: i64 = -9_999_999_999;

pub struct Msg {
    pub kind: u8,
    pub id: u64,
    pub size: u32,
    pub price: i64,
    pub side: Side,
}

fn num<T: FromStr>(s: &str, what: &str) -> Result<T, String> {
    s.trim().parse().map_err(|_| format!("bad {what}: {s:?}"))
}

pub fn parse_message(line: &str) -> Result<Msg, String> {
    let f: Vec<&str> = line.trim().split(',').collect();
    if f.len() < 6 {
        return Err(format!("expected 6 fields, got {}", f.len()));
    }
    let side = match num::<i32>(f[5], "direction")? {
        1 => Side::Bid,
        -1 => Side::Ask,
        d => return Err(format!("direction must be 1 or -1, got {d}")),
    };
    Ok(Msg {
        kind: num(f[1], "type")?,
        id: num(f[2], "order id")?,
        size: num(f[3], "size")?,
        price: num(f[4], "price")?,
        side,
    })
}

/// Raw integers of one orderbook row; length must be a multiple of 4.
pub fn parse_snapshot_row(line: &str) -> Result<Vec<i64>, String> {
    let row = line
        .trim()
        .split(',')
        .map(|s| num::<i64>(s, "orderbook value"))
        .collect::<Result<Vec<_>, _>>()?;
    if row.is_empty() || row.len() % 4 != 0 {
        return Err(format!("orderbook row has {} columns, need a multiple of 4", row.len()));
    }
    Ok(row)
}

pub fn format_snapshot(book: &OrderBook, levels: usize) -> String {
    let asks = book.levels(Side::Ask, levels);
    let bids = book.levels(Side::Bid, levels);
    let mut cols = Vec::with_capacity(levels * 4);
    for i in 0..levels {
        let (ap, asz) = asks.get(i).copied().unwrap_or((EMPTY_ASK, 0));
        let (bp, bsz) = bids.get(i).copied().unwrap_or((EMPTY_BID, 0));
        cols.extend([ap, asz as i64, bp, bsz as i64]);
    }
    cols.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(",")
}