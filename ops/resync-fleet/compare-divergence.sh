#!/usr/bin/env bash
# Run ON a fleet node: compares local arbreth vs canonical Alchemy for block B-1 and B.
# usage: bash -s <block> <alchemy-key>   (piped via gcloud ssh)
B="$1"; KEY="$2"
LOCAL="http://localhost:8545"
CANON="https://arb-sepolia.g.alchemy.com/v2/$KEY"
getblk(){ curl -s -X POST "$1" -H "content-type: application/json" \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_getBlockByNumber\",\"params\":[\"$2\",true]}"; }
{
for off in -1 0 1; do
  n=$((B+off)); hx=$(printf '0x%x' "$n")
  echo "L<<<$(getblk "$LOCAL" "$hx")"
  echo "C<<<$(getblk "$CANON" "$hx")"
  echo "N<<<$n"
done
} | python3 -c '
import sys, json
data=[]; cur={}
for ln in sys.stdin.read().split("\n"):
    if ln.startswith("L<<<"): cur={"L":(json.loads(ln[4:]) or {}).get("result")}
    elif ln.startswith("C<<<"): cur["C"]=(json.loads(ln[4:]) or {}).get("result")
    elif ln.startswith("N<<<"): cur["N"]=int(ln[4:]); data.append(cur)
fields=["hash","parentHash","stateRoot","transactionsRoot","receiptsRoot","gasUsed","gasLimit","timestamp","miner","extraData","logsBloom"]
for d in data:
    L,C,n=d.get("L"),d.get("C"),d["N"]
    print("\n=== block %d ===" % n)
    if not L or not C:
        print("  MISSING:", "ours" if not L else "", "canon" if not C else ""); continue
    for f in fields:
        lv,cv=L.get(f),C.get(f)
        if lv==cv:
            print("   %-16s = %s" % (f, (str(lv)[:20]+".." if f=="logsBloom" and lv else lv)))
        else:
            if f=="logsBloom": print("** %-16s DIFFER" % f)
            else: print("** %-16s ours=%s\n                    canon=%s" % (f, lv, cv))
    lt=L.get("transactions",[]); ct=C.get("transactions",[])
    print("   tx_count         ours=%d canon=%d" % (len(lt), len(ct)))
    # per-tx hash match
    lh=[t.get("hash") for t in lt]; ch=[t.get("hash") for t in ct]
    print("   tx_hashes_match  %s" % (lh==ch))
'
echo ""; echo "=== /data/DIVERGENCE sentinel ==="; cat /data/DIVERGENCE 2>/dev/null
