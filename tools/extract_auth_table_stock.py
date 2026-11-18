#!/usr/bin/env python3
"""Extract the auth-key challenge/response table from a stock SA-MP 0.3.7
server binary (samp03svr).

The stock server does NOT use open.mp's AuthTable: it embeds its own
256-pair table (challenge 12-16 hex chars, response 40 hex chars) as a
contiguous array of 96-byte entries: [challenge: 32 bytes][response: 64
bytes], NUL-padded. The response strings are found by scanning for
40-hex runs spaced exactly 96 bytes apart; the table base is the first
response offset minus 32.

Usage:
    python3 tools/extract_auth_table_stock.py <samp03svr> <out.txt>
The output format matches tools/extract_auth_table.py (one
"CHALLENGE RESPONSE" pair per line) and is read by
samp_proto::auth::AuthTable. Third-party data: do not commit.
"""
import re
import sys

src, dst = sys.argv[1], sys.argv[2]
data = open(src, "rb").read()
resps = [m.start() for m in re.finditer(rb"[0-9A-F]{40}", data)]
# longest run of responses spaced exactly 96 bytes apart
best, run = [], []
for off in resps:
    if run and off - run[-1] == 96:
        run.append(off)
    else:
        if len(run) > len(best):
            best = run
        run = [off]
if len(run) > len(best):
    best = run
if len(best) != 256:
    sys.exit(f"expected a run of 256 responses, found {len(best)}")
base = best[0] - 32
pairs = []
for i in range(256):
    off = base + i * 96
    chal = data[off:off + 32].split(b"\x00")[0].decode()
    resp = data[off + 32:off + 96].split(b"\x00")[0].decode()
    if not re.fullmatch(r"[0-9A-F]{12,16}", chal) or not re.fullmatch(r"[0-9A-F]{40}", resp):
        sys.exit(f"malformed entry {i} at {off:#x}: {chal!r} {resp!r}")
    pairs.append((chal, resp))
if len({c for c, _ in pairs}) != 256:
    sys.exit("challenge strings are not unique")
with open(dst, "w") as f:
    f.write(f"# extracted from {src} (stock SA-MP 0.3.7 server); third-party data, do not commit\n")
    for c, r in pairs:
        f.write(f"{c} {r}\n")
print(f"wrote {len(pairs)} pairs to {dst}")
