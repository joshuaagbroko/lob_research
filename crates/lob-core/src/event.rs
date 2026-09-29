#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Bid,
    Ask,
}

/// Events that change the visible book. Prices are integer ticks.
/// Hidden executions and halts never reach the engine (filter upstream).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Add { order_id: u64, side: Side, price: i64, size: u32 },
    /// Partial cancel: reduces size, keeps queue position.
    Cancel { order_id: u64, size: u32 },
    /// Full cancel: removes the order.
    Delete { order_id: u64 },
    /// Visible execution against a resting order. Partial fills keep queue position.
    Execute { order_id: u64, size: u32 },
}