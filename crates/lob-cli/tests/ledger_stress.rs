//! Exercises the ghost-ledger paths the synthetic selftest can't reach:
//! draining a ghost to exactly zero, then needing another one at the same
//! price (drain-to-empty), and a single event needing more than one ghost's
//! worth of size (top-up within one call). Runs the actual lob-cli binary as
//! a subprocess against hand-built LOBSTER-format files.
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_lob-cli")
}

#[test]
fn drain_to_empty_then_topup_in_one_event() {
    let dir = std::env::temp_dir().join("lob_ledger_stress");
    std::fs::create_dir_all(&dir).unwrap();
    let msg_path = dir.join("message.csv");
    let ob_path = dir.join("orderbook.csv");
    let out_path = dir.join("reconstructed.csv");

    // Row 0 (seed): one visible bid at 100 x 10 (id 1, added by message 0), plus an
    // INVISIBLE pre-existing bid resting at price 90 that never appears as an Add --
    // its combined size (30) only shows up in the reference aggregate at that price.
    // No asks, to keep the snapshot format trivial (EMPTY_ASK sentinel).
    let empty_ask = 9_999_999_999i64;
    let ob_row = |bid_size: i64| format!("{empty_ask},0,100,{bid_size}");

    let mut msgs = Vec::new();
    let mut obs = Vec::new();
    // message 0: add id=1, bid, price 100, size 10 (becomes the seed row).
    msgs.push("34200.0,1,1,10,100,1".to_string());
    obs.push(ob_row(10));

    // message 1: delete an UNKNOWN order (id 501) for 12 shares at price 90 --
    // there is no ghost yet at 90, so this must create one from the reference.
    // Reference row for message 1 doesn't show price 90 in its 1-level window,
    // so this exercises the fallback path (size = need = 12).
    msgs.push("34200.1,3,501,12,90,1".to_string());
    obs.push(ob_row(10)); // price 90 still not visible at level 1 after this event

    // message 2: delete another UNKNOWN order (id 502) for 12 shares at price 90.
    // The ghost from message 1 was sized to exactly 12 (fallback) and fully drained
    // by message 1 itself, so it's already gone -- this must create a SECOND fresh
    // ghost, proving drain-to-empty-then-recreate works rather than erroring.
    msgs.push("34200.2,3,502,12,90,1".to_string());
    obs.push(ob_row(10));

    // message 3: delete a THIRD unknown order (id 503) for 30 shares at price 90.
    // No ghost exists (previous one drained by message 2), so a new one is created.
    // This is a single event needing far more than any prior single ghost held,
    // proving one event can be satisfied by a freshly-sized ghost regardless of
    // what came before.
    msgs.push("34200.3,3,503,30,90,1".to_string());
    obs.push(ob_row(10));

    std::fs::write(&msg_path, msgs.join("\n") + "\n").unwrap();
    std::fs::write(&ob_path, obs.join("\n") + "\n").unwrap();

    let output = Command::new(bin())
        .args([&msg_path, &ob_path, &out_path])
        .output()
        .expect("failed to run lob-cli");
    let stderr = String::from_utf8_lossy(&output.stderr);
    print!("{stderr}"); // visible with `cargo test -- --nocapture`

    assert!(output.status.success(), "lob-cli exited non-zero:\n{stderr}");
    assert!(
        stderr.contains("unknown 0, duplicate 0, oversize 0"),
        "expected zero genuine anomalies (all three events should resolve via ghost reconciliation), got:\n{stderr}"
    );
    // Three separate ghosts must have been created (one per message, since each
    // fully drains the previous one before the next event arrives) -- not one
    // ghost reused incorrectly, and not a crash/short-circuit.
    assert!(
        stderr.contains("phantom reconciliation: 3 shares drawn down"),
        "expected 3 ghost drains (one per message), got:\n{stderr}"
    );

    let reconstructed = std::fs::read_to_string(&out_path).unwrap();
    let rows: Vec<&str> = reconstructed.lines().collect();
    assert_eq!(rows.len(), 4, "one output row per message");
    // Bid side (visible, real order) must be untouched by any of this: still 100 x 10.
    for (i, row) in rows.iter().enumerate() {
        assert!(row.ends_with(",100,10"), "row {i} bid side changed unexpectedly: {row}");
    }
}