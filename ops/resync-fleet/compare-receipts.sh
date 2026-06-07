#!/usr/bin/env bash
# Run ON a fleet node: per-tx receipt/log diff for block B (local arbreth vs canonical).
# usage: bash -s <block> <alchemy-key>
exec python3 - "$1" "$2" <<'PY'
import sys, json, urllib.request
B = int(sys.argv[1]); KEY = sys.argv[2]
LOCAL = "http://localhost:8545"; CANON = "https://arb-sepolia.g.alchemy.com/v2/" + KEY
def rpc(url, method, params):
    req = urllib.request.Request(url, data=json.dumps(
        {"jsonrpc":"2.0","id":1,"method":method,"params":params}).encode(),
        headers={"content-type":"application/json"})
    try: return json.load(urllib.request.urlopen(req, timeout=30)).get("result")
    except Exception as e: return {"_err": str(e)}
blk = rpc(LOCAL, "eth_getBlockByNumber", [hex(B), True])
print("block %d  tx_count=%d" % (B, len(blk.get("transactions", []))))
for i, tx in enumerate(blk.get("transactions", [])):
    h = tx["hash"]
    print("\n-- tx[%d] %s" % (i, h))
    print("   type=%s from=%s to=%s value=%s selector=%s" % (
        tx.get("type"), tx.get("from"), tx.get("to"), tx.get("value"), (tx.get("input") or "0x")[:10]))
    lr = rpc(LOCAL, "eth_getTransactionReceipt", [h]); cr = rpc(CANON, "eth_getTransactionReceipt", [h])
    for f in ["status", "gasUsed", "cumulativeGasUsed", "contractAddress"]:
        lv = (lr or {}).get(f); cv = (cr or {}).get(f)
        print("   %s %-18s ours=%s canon=%s" % ("**" if lv != cv else "  ", f, lv, cv))
    ll = (lr or {}).get("logs", []); cl = (cr or {}).get("logs", [])
    print("   %s logs_count        ours=%d canon=%d" % ("**" if len(ll) != len(cl) else "  ", len(ll), len(cl)))
    for j in range(max(len(ll), len(cl))):
        a = ll[j] if j < len(ll) else None; b = cl[j] if j < len(cl) else None
        if a and b and a.get("address")==b.get("address") and a.get("topics")==b.get("topics") and a.get("data")==b.get("data"):
            continue
        print("      log[%d] DIFFER:" % j)
        if a: print("        ours : addr=%s t0=%s data=%s" % (a.get("address"), (a.get("topics") or ["-"])[0], (a.get("data") or "")[:66]))
        else: print("        ours : <absent>")
        if b: print("        canon: addr=%s t0=%s data=%s" % (b.get("address"), (b.get("topics") or ["-"])[0], (b.get("data") or "")[:66]))
        else: print("        canon: <absent>")
PY
