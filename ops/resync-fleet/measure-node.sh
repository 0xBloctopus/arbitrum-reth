#!/usr/bin/env bash
# measure-node.sh <label> <start-block> [alchemy-key]
# Samples head twice 60s apart (wait runs on the VM), checks the current head hash
# vs canonical Alchemy arb-sepolia, and reads /data/DIVERGENCE. Prints one JSON line.
set -uo pipefail
LABEL="$1"; START="$2"; KEY="${3:-9cCNklIqxb07r-TUavfmxUyjTTDPtvC-}"
VM="arbreth-rs-$LABEL"; Z=us-central1-a; P=bloctopus-dev
CANON_RPC="https://arb-sepolia.g.alchemy.com/v2/$KEY"

REMOTE='
bn(){ curl -s -X POST http://localhost:8545 -H "content-type: application/json" -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_blockNumber\",\"params\":[]}" | python3 -c "import sys,json;print(int(json.load(sys.stdin)[\"result\"],16))" 2>/dev/null; }
bh(){ curl -s -X POST http://localhost:8545 -H "content-type: application/json" -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_getBlockByNumber\",\"params\":[\"$1\",false]}" | python3 -c "import sys,json;print(json.load(sys.stdin)[\"result\"][\"hash\"])" 2>/dev/null; }
t0=$(date +%s); h0=$(bn)
sleep 60
t1=$(date +%s); h1=$(bn); hh1=$(bh $(printf 0x%x ${h1:-0}))
FB=$(cat /data/DIVERGENCE 2>/dev/null | python3 -c "import sys,json;d=json.load(sys.stdin);print(d.get(\"first_bad_block\",\"\"))" 2>/dev/null)
FF=$(cat /data/DIVERGENCE 2>/dev/null | python3 -c "import sys,json;d=json.load(sys.stdin);print(\",\".join(d.get(\"diff\",{}).keys()))" 2>/dev/null)
echo "DATA|${h0}|${h1}|${hh1}|$((t1-t0))|${FB}|${FF}"
'
OUT=$(timeout 115 gcloud compute ssh "$VM" --zone="$Z" --project="$P" --tunnel-through-iap \
  --ssh-flag="-o StrictHostKeyChecking=no -o ConnectTimeout=20" --command="$REMOTE" 2>/dev/null \
  | grep '^DATA|' | tail -1)
IFS='|' read -r _tag H0 H1 HH1 EL FB FF <<<"$OUT"

ch1=""
if [ -n "${H1:-}" ]; then
  ch1=$(curl -s -X POST "$CANON_RPC" -H "content-type: application/json" \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_getBlockByNumber\",\"params\":[\"$(printf 0x%x "$H1")\",false]}" \
    | python3 -c "import sys,json;print(json.load(sys.stdin).get('result',{}).get('hash',''))" 2>/dev/null)
fi
tip=$(curl -s -X POST "$CANON_RPC" -H "content-type: application/json" \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_blockNumber\",\"params\":[]}" \
  | python3 -c "import sys,json;print(int(json.load(sys.stdin)['result'],16))" 2>/dev/null)

python3 - "$LABEL" "$START" "${H0:-}" "${H1:-}" "${HH1:-}" "${EL:-}" "${ch1:-}" "${tip:-}" "${FB:-}" "${FF:-}" <<'PY'
import sys, json
label, start, h0, h1, hh1, el, ch1, tip, fb, ff = sys.argv[1:11]
def I(x):
    try: return int(x)
    except: return None
h0, h1, el, start, tip, fb = I(h0), I(h1), I(el), I(start), I(tip), I(fb)
canon = bool(hh1 and ch1 and hh1 == ch1)
win = (h1 - h0) if (h0 is not None and h1 is not None) else None
sp = round(win / el, 1) if (win is not None and el) else None
note = ""
if h1 is None: note = "RPC unreachable (mid-restart?)"
elif win is not None and win <= 0: note = "no advance in window (mid-restart?)"
print(json.dumps({
  "node": "arbreth-rs-" + label, "start_block": start,
  "head": h1 if h1 is not None else h0,
  "blocks_produced": (h1 - start) if (h1 is not None and start is not None) else None,
  "speed_bps_60s": sp, "canonical": canon,
  "first_bad_block": fb, "divergent_fields": ff or None,
  "pct_to_tip": round(100 * (h1 or h0 or 0) / tip, 3) if tip else None,
  "note": note}))
PY
