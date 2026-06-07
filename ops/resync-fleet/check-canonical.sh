#!/usr/bin/env bash
# Run ON a node: prints head + whether its head hash matches canonical Alchemy.
exec python3 - "$1" <<'PY'
import sys, json, urllib.request
KEY = sys.argv[1]
L = "http://localhost:8545"; C = "https://arb-sepolia.g.alchemy.com/v2/" + KEY
def rpc(u, m, p):
    r = urllib.request.Request(u, data=json.dumps({"jsonrpc":"2.0","id":1,"method":m,"params":p}).encode(), headers={"content-type":"application/json"})
    try: return json.load(urllib.request.urlopen(r, timeout=20)).get("result")
    except Exception: return None
h = rpc(L, "eth_blockNumber", [])
if not h: print("head=? canonical=UNKNOWN(rpc down)"); sys.exit()
h = int(h, 16)
lb = rpc(L, "eth_getBlockByNumber", [hex(h), False]); cb = rpc(C, "eth_getBlockByNumber", [hex(h), False])
lh = lb.get("hash") if lb else None; ch = cb.get("hash") if cb else None
print("head=%d canonical=%s (ours=%s canon=%s)" % (h, (lh == ch and lh is not None), (lh or '-')[:14], (ch or '-')[:14]))
PY
