"""Diff the reconstructed book against LOBSTER's own orderbook file.

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
    if levels:
        ref, mine = ref[:, : 4 * levels], mine[:, : 4 * levels]
    if ref.shape != mine.shape:
        print(f"FAIL: shape mismatch, reference {ref.shape} vs reconstructed {mine.shape}")
        return 1
    bad = np.argwhere(ref != mine)
    if len(bad):
        row, col = bad[0]
        print(f"FAIL: {len(np.unique(bad[:, 0]))} mismatching rows; first at row {row}, column {col}: "
              f"reference {ref[row, col]} vs reconstructed {mine[row, col]}")
        return 1
    print(f"OK: {ref.shape[0]} rows x {ref.shape[1]} columns identical")
    return 0


if __name__ == "__main__":
    if len(sys.argv) not in (3, 4):
        sys.exit(__doc__)
    sys.exit(main(sys.argv[1], sys.argv[2], int(sys.argv[3]) if len(sys.argv) == 4 else None))