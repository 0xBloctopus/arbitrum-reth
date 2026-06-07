#!/usr/bin/env bash
# Create one resync-fleet VM: n2/n2d-standard-32, 128GB, N x 375GB local NVMe.
#   ./create-vm.sh <label> <snapshot-block|genesis> [n2|n2d] [num_local_ssd]
# Per-family local-SSD quota is 20TB (=53 disks); right-size num_local_ssd to
# the snapshot's start size + growth headroom (8=3TB, 12=4.5TB, 16=6TB).
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

LABEL="$1"; SNAPSHOT="$2"; FAMILY="${3:-n2}"; NSSD="${4:-16}"
PROJECT=bloctopus-dev
ZONE=us-central1-a
SA=254824854621-compute@developer.gserviceaccount.com
TAG=v0.1.1
STATUS_BUCKET=gs://arbreth-resync-status

KEY="${ALCHEMY_KEY:-}"
if [ -z "$KEY" ] && [ -f "$HERE/../../.env.alchemy" ]; then
  KEY=$(grep -E '^ETH_SEPOLIA_RPC=' "$HERE/../../.env.alchemy" | sed -E 's#.*/v2/##')
fi
[ -n "$KEY" ] || { echo "ERROR: no Alchemy key (set ALCHEMY_KEY or .env.alchemy)"; exit 1; }

MT="${FAMILY}-standard-32"
NAME="arbreth-rs-${LABEL}"
ssd_flags=(); for _ in $(seq 1 "$NSSD"); do ssd_flags+=(--local-ssd=interface=NVME); done

echo "creating $NAME ($MT, ${NSSD}x375GB local NVMe, snapshot=$SNAPSHOT)"
gcloud compute instances create "$NAME" \
  --project="$PROJECT" --zone="$ZONE" \
  --machine-type="$MT" \
  --image-family=ubuntu-2404-lts-amd64 --image-project=ubuntu-os-cloud \
  --boot-disk-size=50GB --boot-disk-type=pd-balanced \
  "${ssd_flags[@]}" \
  --service-account="$SA" \
  --scopes=https://www.googleapis.com/auth/devstorage.read_write,https://www.googleapis.com/auth/logging.write,https://www.googleapis.com/auth/monitoring.write \
  --tags=arbreth-resync \
  --metadata=node-name="$NAME",snapshot-block="$SNAPSHOT",alchemy-key="$KEY",arbreth-tag="$TAG",status-bucket="$STATUS_BUCKET" \
  --metadata-from-file=startup-script="$HERE/startup.sh"
