#!/usr/bin/env bash
# Low-noise resync-fleet watcher. Emits an event line only on:
#   - a node first reaching OK (online)         [once per node]
#   - any node entering DIVERGENCE              [once per node]
#   - a 30-minute heartbeat summary
# Authoritative detection is VM-side (gs://arbreth-resync-status + /data/DIVERGENCE).
set -uo pipefail
BUCKET="${STATUS_BUCKET:-gs://arbreth-resync-status}"
STATE="$(mktemp -d)"
POLL=180          # status poll cadence (divergence/online detection)
HB_EVERY=60       # heartbeat every HB_EVERY polls (60*180s = 3h)
first=1
hb=0
while true; do
  TMP="$(mktemp -d)"
  gcloud storage cp "$BUCKET/*.json" "$TMP/" 2>/dev/null || true
  alert=""; summary=""
  for f in "$TMP"/*.json; do
    [ -f "$f" ] || continue
    node="$(basename "$f" .json)"
    vals="$(python3 -c "
import json
d=json.load(open('$f'))
print(d.get('status','?'), d.get('head','-'), d.get('pct_to_tip','-'), d.get('blocks_per_sec','-'), d.get('first_bad_block','-'), ','.join(d.get('diff',{}).keys()) or '-')
" 2>/dev/null)" || continue
    set -- $vals; st="$1"; head="$2"; pct="$3"; bps="$4"; fb="$5"; fields="$6"
    summary+="${node#arbreth-rs-}:${st}@${head}(${pct}%) "
    if { [ "$st" = "DIVERGENCE" ] || [ "$st" = "DIVERGED_HALTED" ]; } && [ ! -f "$STATE/div-$node" ]; then
      touch "$STATE/div-$node"
      [ "$first" = 0 ] && alert+=$'\n'"🚨 DIVERGENCE $node  first_bad_block=${fb}  fields=${fields}  (head=$head) — pull /data/DIVERGENCE for full diff"
    fi
    if { [ "$st" = "OK" ] || [ "$st" = "DIVERGED_HALTED" ]; } && [ ! -f "$STATE/up-$node" ]; then
      touch "$STATE/up-$node"
      [ "$first" = 0 ] && alert+=$'\n'"✅ ${node#arbreth-rs-} online — ${bps} blk/s at head $head"
    fi
  done
  rm -rf "$TMP"
  [ -n "$alert" ] && echo "[$(date -u +%H:%M)]$alert"
  hb=$((hb+1))
  if [ $((hb % HB_EVERY)) -eq 1 ]; then echo "[$(date -u +%H:%M) heartbeat] ${summary:-(no statuses yet)}"; fi
  first=0
  sleep "$POLL"
done
