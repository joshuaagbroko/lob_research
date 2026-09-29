use std::collections::{BTreeMap, HashMap, VecDeque};

use crate::event::{Event, Side};

#[derive(Debug, PartialEq, Eq)]
pub enum BookError {
    UnknownOrder(u64),
    DuplicateOrder(u64),
    /// Cancel/execute size exceeds the resting size.
    Oversize(u64),
}

struct Order {
    id: u64,
    size: u32,
}

/// FIFO queue at one price. Queue position == index in `orders`.
#[derive(Default)]
struct Level {
    orders: VecDeque<Order>,
    total: u32,
}

#[derive(Default)]
pub struct OrderBook {
    bids: BTreeMap<i64, Level>,
    asks: BTreeMap<i64, Level>,
    /// order_id -> (side, price): O(1) lookup for cancel/execute.
    index: HashMap<u64, (Side, i64)>,
}

impl OrderBook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply(&mut self, ev: Event) -> Result<(), BookError> {
        match ev {
            Event::Add { order_id, side, price, size } => {
                if self.index.contains_key(&order_id) {
                    return Err(BookError::DuplicateOrder(order_id));
                }
                let level = self.side_mut(side).entry(price).or_default();
                level.orders.push_back(Order { id: order_id, size });
                level.total += size;
                self.index.insert(order_id, (side, price));
                Ok(())
            }
            Event::Cancel { order_id, size } | Event::Execute { order_id, size } => {
                self.reduce(order_id, size)
            }
            Event::Delete { order_id } => {
                let (side, price) = *self
                    .index
                    .get(&order_id)
                    .ok_or(BookError::UnknownOrder(order_id))?;
                let level = self.side_mut(side).get_mut(&price).expect("index/book out of sync");
                let pos = level.orders.iter().position(|o| o.id == order_id).expect("index/book out of sync");
                let removed = level.orders.remove(pos).expect("position just found");
                level.total -= removed.size;
                self.finish_removal(order_id, side, price);
                Ok(())
            }
        }
    }

    fn reduce(&mut self, order_id: u64, size: u32) -> Result<(), BookError> {
        let (side, price) = *self
            .index
            .get(&order_id)
            .ok_or(BookError::UnknownOrder(order_id))?;
        let level = self.side_mut(side).get_mut(&price).expect("index/book out of sync");
        let pos = level.orders.iter().position(|o| o.id == order_id).expect("index/book out of sync");
        let order = &mut level.orders[pos];
        if size > order.size {
            return Err(BookError::Oversize(order_id));
        }
        // In-place size change: the order keeps its slot, so queue position is preserved.
        order.size -= size;
        level.total -= size;
        if order.size == 0 {
            level.orders.remove(pos);
            self.finish_removal(order_id, side, price);
        }
        Ok(())
    }

    fn finish_removal(&mut self, order_id: u64, side: Side, price: i64) {
        self.index.remove(&order_id);
        let map = self.side_mut(side);
        if map.get(&price).is_some_and(|l| l.orders.is_empty()) {
            map.remove(&price);
        }
    }

    fn side_mut(&mut self, side: Side) -> &mut BTreeMap<i64, Level> {
        match side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        }
    }

    /// (price, total size) of the best bid.
    pub fn best_bid(&self) -> Option<(i64, u32)> {
        self.bids.iter().next_back().map(|(p, l)| (*p, l.total))
    }

    /// (price, total size) of the best ask.
    pub fn best_ask(&self) -> Option<(i64, u32)> {
        self.asks.iter().next().map(|(p, l)| (*p, l.total))
    }

    /// Top `n` levels, best first: bids descending, asks ascending.
    pub fn levels(&self, side: Side, n: usize) -> Vec<(i64, u32)> {
        match side {
            Side::Bid => self.bids.iter().rev().take(n).map(|(p, l)| (*p, l.total)).collect(),
            Side::Ask => self.asks.iter().take(n).map(|(p, l)| (*p, l.total)).collect(),
        }
    }

    /// Total size queued ahead of `order_id` at its price level (O(level length)).
    pub fn queue_ahead(&self, order_id: u64) -> Option<u32> {
        let (side, price) = *self.index.get(&order_id)?;
        let level = match side {
            Side::Bid => self.bids.get(&price)?,
            Side::Ask => self.asks.get(&price)?,
        };
        Some(level.orders.iter().take_while(|o| o.id != order_id).map(|o| o.size).sum())
    }
}