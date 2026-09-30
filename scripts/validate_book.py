"""Compare reconstructed book against LOBSTER reference, level by level.

Usage: python scripts/validate_book.py <lobster_orderbook.csv> <reconstructed.csv> [levels]

Optional `levels` compares only the top N levels (orders resting deeper than the file's
visible depth at market open are unknowable, so deep-level drift is expected on real data).

Both files use LOBSTER's orderbook layout (no header; per level:
ask_price, ask_size, bid_price, bid_size) with one row per message.
Exits non-zero on the first mismatch.
"""
import sys

import numpy as np


def main(ref_path: str, mine_path: str, levels: int | None = None) -> int:
    ref = np.loadtxt(ref_path, delimiter=",", dtype=np.int64)
    mine = np.loadtxt(mine_path, delimiter=",", dtype=np.int64)

    if ref.shape != mine.shape:
        print(f"FAIL: shape mismatch, reference {ref.shape} vs reconstructed {mine.shape}")
        return 1

    n_cols = ref.shape[1]
    if levels is None:
        levels = n_cols // 4

    overall_first = None
    for lvl in range(levels):
        cols = slice(4 * lvl, 4 * lvl + 4)
        diff_rows = np.argwhere((ref[:, cols] != mine[:, cols]).any(axis=1)).ravel()
        n_diff = len(diff_rows)
        if n_diff == 0:
            print(f"level {lvl+1:2d}: OK — {ref.shape[0]} rows identical")
            continue
        first = int(diff_rows[0])
        if overall_first is None or first < overall_first:
            overall_first = first
        print(f"level {lvl+1:2d}: {n_diff:>7d} / {ref.shape[0]} rows differ, "
              f"first at row {first}")

    if overall_first is None:
        print(f"\nOK: {ref.shape[0]} rows × {ref.shape[1]} columns identical")
        return 0
    print(f"\nFAIL: earliest divergence at row {overall_first}")
    return 1


if __name__ == "__main__":
    ref_path, mine_path = sys.argv[1], sys.argv[2]
    levels = int(sys.argv[3]) if len(sys.argv) > 3 else None
    sys.exit(main(ref_path, mine_path, levels))