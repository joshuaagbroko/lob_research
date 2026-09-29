use lob_core::{BookError, Event, OrderBook, Side};

fn add(b: &mut OrderBook, id: u64, side: Side, price: i64, size: u32) {
    b.apply(Event::Add { order_id: id, side, price, size }).unwrap();
}

#[test]
fn best_prices_and_level_ordering() {
    let mut b = OrderBook::new();
    add(&mut b, 1, Side::Bid, 100, 10);
    add(&mut b, 2, Side::Bid, 99, 20);
    add(&mut b, 3, Side::Ask, 101, 5);
    add(&mut b, 4, Side::Ask, 102, 7);
    assert_eq!(b.best_bid(), Some((100, 10)));
    assert_eq!(b.best_ask(), Some((101, 5)));
    assert_eq!(b.levels(Side::Bid, 5), vec![(100, 10), (99, 20)]);
    assert_eq!(b.levels(Side::Ask, 5), vec![(101, 5), (102, 7)]);
}

#[test]
fn fifo_queue_ahead() {
    let mut b = OrderBook::new();
    add(&mut b, 1, Side::Bid, 100, 100);
    add(&mut b, 2, Side::Bid, 100, 50);
    add(&mut b, 3, Side::Bid, 100, 25);
    assert_eq!(b.queue_ahead(1), Some(0));
    assert_eq!(b.queue_ahead(2), Some(100));
    assert_eq!(b.queue_ahead(3), Some(150));
}

#[test]
fn partial_fill_preserves_queue_position() {
    let mut b = OrderBook::new();
    add(&mut b, 1, Side::Bid, 100, 100);
    add(&mut b, 2, Side::Bid, 100, 50);
    b.apply(Event::Execute { order_id: 1, size: 30 }).unwrap();
    assert_eq!(b.queue_ahead(1), Some(0), "partially filled order stays at the front");
    assert_eq!(b.queue_ahead(2), Some(70));
    assert_eq!(b.best_bid(), Some((100, 120)));
}

#[test]
fn full_fill_promotes_next_order() {
    let mut b = OrderBook::new();
    add(&mut b, 1, Side::Ask, 101, 10);
    add(&mut b, 2, Side::Ask, 101, 40);
    b.apply(Event::Execute { order_id: 1, size: 10 }).unwrap();
    assert_eq!(b.queue_ahead(1), None);
    assert_eq!(b.queue_ahead(2), Some(0));
}

#[test]
fn partial_cancel_preserves_position_and_delete_removes_level() {
    let mut b = OrderBook::new();
    add(&mut b, 1, Side::Bid, 100, 100);
    add(&mut b, 2, Side::Bid, 100, 50);
    b.apply(Event::Cancel { order_id: 1, size: 40 }).unwrap();
    assert_eq!(b.queue_ahead(1), Some(0));
    assert_eq!(b.queue_ahead(2), Some(60));
    b.apply(Event::Delete { order_id: 1 }).unwrap();
    b.apply(Event::Delete { order_id: 2 }).unwrap();
    assert_eq!(b.best_bid(), None, "empty level must be removed, not left at size 0");
}

#[test]
fn rejects_bad_events() {
    let mut b = OrderBook::new();
    add(&mut b, 1, Side::Bid, 100, 10);
    assert_eq!(b.apply(Event::Delete { order_id: 9 }), Err(BookError::UnknownOrder(9)));
    assert_eq!(b.apply(Event::Execute { order_id: 1, size: 11 }), Err(BookError::Oversize(1)));
    assert_eq!(
        b.apply(Event::Add { order_id: 1, side: Side::Bid, price: 100, size: 1 }),
        Err(BookError::DuplicateOrder(1))
    );
    assert_eq!(b.best_bid(), Some((100, 10)), "failed events must not mutate the book");
}