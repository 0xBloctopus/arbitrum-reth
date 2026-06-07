#!/usr/bin/env bash
# Run ON a fleet VM as root (sudo bash -s <snapshot-block>): stop the node, wipe the
# fork DB, restore a clean snapshot, and leave the node STOPPED (staged for a fixed image).
set -uo pipefail
SNAP="$1"
SNAP_BUCKET=gs://arbreth-snapshots
echo "[stage] $(hostname): stopping node + timers"
systemctl stop arbreth-restart.timer arbreth-monitor 2>/dev/null || true
(cd /opt/arbreth && docker compose down) 2>/dev/null || true
echo "[stage] wiping /data DB"
rm -rf /data/arbreth-data /data/nitro-data /data/snapshot.tar.zst /data/DIVERGENCE /data/status.json
echo "[stage] downloading snapshot ${SNAP}"
time gcloud storage cp "${SNAP_BUCKET}/arbreth-sepolia-${SNAP}.tar.zst" /data/snapshot.tar.zst
echo "[stage] extracting"
time bash -c "zstd -dc -T0 /data/snapshot.tar.zst | tar -xf - -C /data"
rm -f /data/snapshot.tar.zst
[ -f /data/arbreth-data/jwt.hex ] || openssl rand -hex 32 > /data/arbreth-data/jwt.hex
echo "[stage] STAGED at snapshot ${SNAP}, node STOPPED. /data:"
df -h /data | tail -1
ls -d /data/arbreth-data /data/nitro-data 2>/dev/null
echo "[stage] DONE ${SNAP}"
