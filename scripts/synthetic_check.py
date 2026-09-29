"""Generate a synthetic LOBSTER-format message + orderbook pair from a deliberately naive,
independent reference book (dict of lists), including pre-existing orders and hidden executions.
Used by `make selftest` to cross-check lob-cli/lob-core against a second implementation.

Usage: python scripts/synthetic_check.py <out_dir>
"""
import os
import random
import sys

random.seed(7)
TICK, MID, LEVELS, N = 100, 5_000_000, 10, 3000
EMPTY_ASK, EMPTY_BID = 9_999_999_999, -9_999_999_999
book = {1: {}, -1: {}}  # direction -> price -> [[order_id, size], ...] in arrival order


def snapshot():
    bids = sorted(book[1], reverse=True)[:LEVELS]
    asks = sorted(book[-1])[:LEVELS]
    row = []
    for i in range(LEVELS):
        row += [asks[i], sum(o[1] for o in book[-1][asks[i]])] if i < len(asks) else [EMPTY_ASK, 0]
        row += [bids[i], sum(o[1] for o in book[1][bids[i]])] if i < len(bids) else [EMPTY_BID, 0]
    return ",".join(map(str, row))


def put(side, price, oid, size):
    book[side].setdefault(price, []).append([oid, size])


def drop_if_empty(side, price):
    if not book[side][price]:
        del book[side][price]


def all_orders():
    return [(s, p, o) for s in (1, -1) for p, os_ in book[s].items() for o in os_]


# Pre-existing orders (ids 1000+): never added inside the file, only referenced later.
pre_id = 1000
for k in range(1, 9):
    for _ in range(random.randint(1, 3)):
        put(1, MID - k * TICK, pre_id, random.randint(50, 300)); pre_id += 1
        put(-1, MID + k * TICK, pre_id, random.randint(50, 300)); pre_id += 1

msgs, obs, t, next_id = [], [], 34200.0, 10_000


def emit(kind, oid, size, price, side):
    global t
    t += random.random() * 0.01
    msgs.append(f"{t:.9f},{kind},{oid},{size},{price},{side}")
    obs.append(snapshot())


put(1, MID - TICK, 1, 100)
emit(1, 1, 100, MID - TICK, 1)  # message 0

for _ in range(N):
    bb, ba = max(book[1]), min(book[-1])
    r = random.random()
    if r < 0.40:
        side = random.choice((1, -1))
        price = random.randrange(bb - 4 * TICK, ba, TICK) if side == 1 else random.randrange(bb + TICK, ba + 5 * TICK, TICK)
        size = random.randint(10, 200)
        put(side, price, next_id, size); emit(1, next_id, size, price, side); next_id += 1
    elif r < 0.55:
        cands = [(s, p, o) for s, p, o in all_orders() if o[1] > 1]
        s, p, o = random.choice(cands)
        c = random.randint(1, o[1] - 1)
        o[1] -= c; emit(2, o[0], c, p, s)
    elif r < 0.70:
        s, p, o = random.choice(all_orders())
        book[s][p].remove(o); drop_if_empty(s, p); emit(3, o[0], o[1], p, s)
    elif r < 0.90:
        s = random.choice((1, -1))
        p = max(book[1]) if s == 1 else min(book[-1])
        o = book[s][p][0]
        q = random.randint(1, o[1])
        o[1] -= q
        if o[1] == 0:
            book[s][p].pop(0); drop_if_empty(s, p)
        emit(4, o[0], q, p, s)
    else:
        emit(5, 999_999, 10, MID, random.choice((1, -1)))  # hidden execution: book unchanged

os.makedirs(sys.argv[1], exist_ok=True)
open(os.path.join(sys.argv[1], "message.csv"), "w").write("\n".join(msgs) + "\n")
open(os.path.join(sys.argv[1], "orderbook.csv"), "w").write("\n".join(obs) + "\n")
print(f"wrote {len(msgs)} messages to {sys.argv[1]}")