#!/usr/bin/env bash
# Aggregate resync-fleet status from GCS. Highlights any DIVERGENCE.
set -uo pipefail
STATUS_BUCKET="${STATUS_BUCKET:-gs://arbreth-resync-status}"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

gcloud storage cp "$STATUS_BUCKET/*.json" "$TMP/" 2>/dev/null || true

printf "%-22s %-16s %13s %13s %8s %8s  %s\n" NODE STATUS HEAD TIP PCT BLK/S NOTE
ls "$TMP"/*.json >/dev/null 2>&1 || { echo "(no status objects yet)"; exit 0; }
for f in "$TMP"/*.json; do
  python3 - "$f" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
st = d.get("status", "?")
note = ""
if st == "DIVERGENCE":
    note = "FIRST_BAD=%s fields=%s" % (d.get("first_bad_block"), ",".join(d.get("diff", {}).keys()))
print("%-22s %-16s %13s %13s %7s%% %8s  %s" % (
    d.get("node", "?"), st, d.get("head", "-"), d.get("canonical_tip", "-"),
    d.get("pct_to_tip", "-"), d.get("blocks_per_sec", "-"), note))
PY
done

if grep -lq DIVERGENCE "$TMP"/*.json 2>/dev/null; then
  echo ""
  echo ">>> DIVERGENCE DETECTED <<<  full diff:"
  for f in $(grep -l DIVERGENCE "$TMP"/*.json 2>/dev/null); do echo "--- $f ---"; cat "$f"; done
fi
