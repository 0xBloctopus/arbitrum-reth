#!/usr/bin/env bash
# Arbitrum-Reth resync-fleet node provisioner (GCP startup-script).
# Templated by instance metadata; idempotent across reboots. All node data
# lives on a RAID0 of the bundled local NVMe (never network disk).
set -uo pipefail
exec > >(tee -a /var/log/arbreth-provision.log) 2>&1
echo "===== arbreth resync provision @ $(date -u) ====="

MARK=/var/lib/arbreth-provisioned
meta(){ curl -s -H "Metadata-Flavor: Google" "http://metadata.google.internal/computeMetadata/v1/instance/attributes/$1"; }

NODE=$(meta node-name)
SNAPSHOT=$(meta snapshot-block)        # "genesis" or a block number
ALCHEMY_KEY=$(meta alchemy-key)
ARBRETH_TAG=$(meta arbreth-tag)
STATUS_BUCKET=$(meta status-bucket)    # e.g. gs://arbreth-resync-status
SNAP_BUCKET=gs://arbreth-snapshots
echo "node=$NODE snapshot=$SNAPSHOT tag=$ARBRETH_TAG status=$STATUS_BUCKET"

ensure_mounted(){ mountpoint -q /data || { mdadm --assemble --scan 2>/dev/null || true; mount /dev/md0 /data 2>/dev/null || true; }; }

# ---- subsequent boots: never rebuild RAID, just bring services back ----
if [ -f "$MARK" ]; then
  echo "already provisioned; ensuring mount + services"
  ensure_mounted
  (cd /opt/arbreth && docker compose up -d) || true
  systemctl start arbreth-monitor 2>/dev/null || true
  exit 0
fi

export DEBIAN_FRONTEND=noninteractive

# ---- 1. packages ----
if ! command -v docker >/dev/null 2>&1; then
  apt-get update -y
  apt-get install -y ca-certificates curl gnupg zstd mdadm jq python3 openssl lsb-release
  install -m 0755 -d /etc/apt/keyrings
  curl -fsSL https://download.docker.com/linux/ubuntu/gpg | gpg --dearmor -o /etc/apt/keyrings/docker.gpg
  chmod a+r /etc/apt/keyrings/docker.gpg
  echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.gpg] https://download.docker.com/linux/ubuntu $(lsb_release -cs) stable" \
    > /etc/apt/sources.list.d/docker.list
  apt-get update -y
  apt-get install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
fi
command -v gcloud >/dev/null 2>&1 || snap install google-cloud-cli --classic

# ---- 2. RAID0 over the bundled local NVMe (375G devices) ----
if ! mountpoint -q /data; then
  mapfile -t SSDS < <(lsblk -dpno NAME,SIZE,TYPE | awk '$3=="disk" && $2=="375G"{print $1}')
  echo "local NVMe devices (${#SSDS[@]}): ${SSDS[*]}"
  if [ "${#SSDS[@]}" -lt 1 ]; then echo "FATAL: no local NVMe found"; exit 1; fi
  mdadm --create /dev/md0 --level=0 --force --run --raid-devices="${#SSDS[@]}" "${SSDS[@]}"
  mkfs.ext4 -F -m 0 /dev/md0
  mkdir -p /data
  mount -o noatime,nodiratime /dev/md0 /data
  mdadm --detail --scan >> /etc/mdadm/mdadm.conf
  grep -q "/dev/md0 /data" /etc/fstab || echo "/dev/md0 /data ext4 noatime,nodiratime,nofail 0 0" >> /etc/fstab
  update-initramfs -u 2>/dev/null || true
fi
df -hT /data

# ---- 3. kernel writeback tuning (OPERATIONS.md) ----
cat >/etc/sysctl.d/99-arbreth.conf <<'EOF'
vm.dirty_ratio = 10
vm.dirty_background_ratio = 5
vm.dirty_writeback_centisecs = 3000
vm.dirty_expire_centisecs = 6000
EOF
sysctl --system >/dev/null 2>&1 || true

# keep docker images off the 50G boot disk
if [ ! -d /data/docker ]; then
  mkdir -p /data/docker
  mkdir -p /etc/docker
  echo '{ "data-root": "/data/docker" }' > /etc/docker/daemon.json
  systemctl restart docker || true
fi

# ---- 4. snapshot restore (download -> parallel decompress -> delete tar) ----
mkdir -p /data
if [ "$SNAPSHOT" != "genesis" ]; then
  echo "downloading snapshot $SNAPSHOT ..."
  time gcloud storage cp "$SNAP_BUCKET/arbreth-sepolia-${SNAPSHOT}.tar.zst" /data/snapshot.tar.zst
  echo "extracting ..."
  time bash -c "zstd -dc -T0 /data/snapshot.tar.zst | tar -xf - -C /data"
  rm -f /data/snapshot.tar.zst
fi
mkdir -p /data/arbreth-data /data/nitro-data
[ -f /data/arbreth-data/jwt.hex ] || openssl rand -hex 32 > /data/arbreth-data/jwt.hex
chmod 644 /data/arbreth-data/jwt.hex

# ---- 5. compose + env ----
mkdir -p /opt/arbreth
cat >/opt/arbreth/.env <<EOF
PARENT_CHAIN_RPC_URL=https://eth-sepolia.g.alchemy.com/v2/${ALCHEMY_KEY}
PARENT_CHAIN_BEACON_URL=https://eth-sepoliabeacon.g.alchemy.com/v2/${ALCHEMY_KEY}
CHAIN_ID=421614
NITRO_IMAGE=offchainlabs/nitro-node:v3.10.0-rc.10-b1cf6db
ARBRETH_TAG=${ARBRETH_TAG}
FEED_URL=wss://sepolia-rollup.arbitrum.io/feed
LOG_LEVEL=WARN
RUST_LOG=warn
DATA_DIR=/data/arbreth-data
NITRO_DATA_DIR=/data/nitro-data
ARB_FLUSH_INTERVAL=64
EOF

cat >/opt/arbreth/docker-compose.yml <<'YAML'
services:
  arbitrum-reth:
    image: ghcr.io/0xbloctopus/arbitrum-reth:${ARBRETH_TAG:-latest}
    pull_policy: always
    container_name: arbitrum-reth
    restart: unless-stopped
    ports:
      - "8545:8545"
      - "8551:8551"
    volumes:
      - ${DATA_DIR:-arbitrum-reth-data}:/data
    environment:
      - RUST_LOG=${RUST_LOG:-warn}
      - ARB_BLOCK_BUFFER_SIZE=${ARB_BLOCK_BUFFER_SIZE:-128}
      - ARB_FLUSH_INTERVAL=${ARB_FLUSH_INTERVAL:-128}
    command:
      - node
      - --chain=/genesis/arbitrum-sepolia.json
      - --datadir=/data/db
      - --http
      - --http.addr=0.0.0.0
      - --http.port=8545
      - --http.api=eth,web3,net,debug
      - --authrpc.addr=0.0.0.0
      - --authrpc.port=8551
      - --authrpc.jwtsecret=/data/jwt.hex
      - --disable-discovery
      - --log.stdout.filter=${RUST_LOG:-warn}
      - --db.exclusive=true
      - --db.growth-step=4GB
      - --db.log-level=error
      - --db.sync-mode=safe-no-sync
    networks: [arb-network]
    logging:
      driver: json-file
      options: { max-size: "50m", max-file: "3" }
    healthcheck:
      test: ["CMD-SHELL", "timeout 5 bash -c '</dev/tcp/localhost/8551' || exit 1"]
      interval: 10s
      timeout: 5s
      retries: 10
      start_period: 30s

  nitro:
    image: ${NITRO_IMAGE:-offchainlabs/nitro-node:v3.10.0-rc.10-b1cf6db}
    container_name: nitro
    user: root
    entrypoint: /usr/local/bin/nitro
    depends_on:
      arbitrum-reth:
        condition: service_healthy
    restart: unless-stopped
    volumes:
      - ${NITRO_DATA_DIR:-nitro-data}:/tmp/nitro-data
      - ${DATA_DIR:-arbitrum-reth-data}:/arbitrum-reth-data:ro
    command:
      - --init.empty=true
      - --init.validate-genesis-assertion=false
      - --persistent.global-config=/tmp/nitro-data
      - --parent-chain.connection.url=${PARENT_CHAIN_RPC_URL}
      - --parent-chain.blob-client.beacon-url=${PARENT_CHAIN_BEACON_URL}
      - --chain.id=${CHAIN_ID:-421614}
      - --execution.forwarding-target=null
      - --log-level=${LOG_LEVEL:-WARN}
      - --node.execution-rpc-client.url=http://arbitrum-reth:8551
      - --node.execution-rpc-client.jwtsecret=/arbitrum-reth-data/jwt.hex
      - --node.sequencer=false
      - --node.feed.input.url=${FEED_URL:-wss://sepolia-rollup.arbitrum.io/feed}
    networks: [arb-network]
    logging:
      driver: json-file
      options: { max-size: "50m", max-file: "3" }

networks:
  arb-network:
    name: arb-network
YAML

cd /opt/arbreth
docker compose pull
docker compose up -d

# ---- 6. divergence monitor ----
cat >/opt/arbreth/monitor.py <<'PYEOF'
#!/usr/bin/env python3
"""Head-hash divergence monitor vs canonical Arbitrum Sepolia.

Arbitrum headers chain parentHash, so a matching head hash proves the whole
synced prefix is byte-identical to canonical. On any mismatch we bisect the
[last_good, head] range to pin the first divergent block, dump the field diff,
drop a /data/DIVERGENCE sentinel, and keep publishing status.
"""
import json, os, subprocess, time, tempfile

NODE = os.environ.get("NODE_NAME", "node")
KEY = os.environ.get("ALCHEMY_KEY", "")
STATUS_BUCKET = os.environ.get("STATUS_BUCKET", "").rstrip("/")
START = os.environ.get("SNAPSHOT_BLOCK", "genesis")
START_BLOCK = 0 if START == "genesis" else int(START)
POLL = float(os.environ.get("POLL", "60"))

ARB = "http://localhost:8545"
CANON = "https://arb-sepolia.g.alchemy.com/v2/%s" % KEY
FIELDS = ["hash", "stateRoot", "transactionsRoot", "receiptsRoot", "gasUsed", "extraData"]
LOCAL_STATUS = "/data/status.json"
SENTINEL = "/data/DIVERGENCE"


def rpc(url, method, params, retries=4):
    payload = json.dumps({"jsonrpc": "2.0", "method": method, "params": params, "id": 1})
    for i in range(retries):
        try:
            p = subprocess.run(
                ["curl", "-s", "-X", "POST", url, "-H", "Content-Type: application/json",
                 "-d", payload, "--max-time", "30"],
                capture_output=True, text=True, timeout=35)
            if p.returncode == 0 and p.stdout.strip():
                r = json.loads(p.stdout)
                if "result" in r:
                    return r["result"]
        except Exception:
            pass
        time.sleep(1.5 * (i + 1))
    return None


def head(url):
    r = rpc(url, "eth_blockNumber", [])
    try:
        return int(r, 16)
    except Exception:
        return None


def block(url, n):
    return rpc(url, "eth_getBlockByNumber", [hex(n), False])


def compare(n):
    a = block(ARB, n)
    c = block(CANON, n)
    if not a or not c:
        return None
    diff = {f: {"ours": a.get(f), "canonical": c.get(f)} for f in FIELDS if a.get(f) != c.get(f)}
    return (len(diff) == 0, diff)


def first_divergence(lo, hi):
    while lo + 1 < hi:
        mid = (lo + hi) // 2
        r = compare(mid)
        if r is None:
            time.sleep(2)
            continue
        if r[0]:
            lo = mid
        else:
            hi = mid
    return hi


def write_status(d):
    d["node"] = NODE
    d["ts"] = int(time.time())
    txt = json.dumps(d, indent=2)
    try:
        with open(LOCAL_STATUS, "w") as f:
            f.write(txt)
    except Exception:
        pass
    if STATUS_BUCKET:
        try:
            with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as tf:
                tf.write(txt)
                tmp = tf.name
            subprocess.run(["gcloud", "storage", "cp", "-q", tmp, "%s/%s.json" % (STATUS_BUCKET, NODE)],
                           timeout=60, capture_output=True)
            os.unlink(tmp)
        except Exception:
            pass


def main():
    start_verified = None
    if START_BLOCK > 0:
        r = compare(START_BLOCK)
        if r is not None:
            start_verified = r[0]
    last_good = START_BLOCK
    w_h0 = None
    w_t0 = time.time()
    diverged = False
    while True:
        h = head(ARB)
        ct = head(CANON)
        if h is None:
            write_status({"status": "WAITING_RPC", "start_block": START_BLOCK,
                          "start_verified": start_verified})
            time.sleep(15)
            continue
        if w_h0 is None:
            w_h0, w_t0 = h, time.time()
        r = compare(h)
        if r is None:
            time.sleep(10)
            continue
        match = r[0]
        rate = (h - w_h0) / max(1.0, time.time() - w_t0)
        base = {"start_block": START_BLOCK, "start_verified": start_verified,
                "head": h, "canonical_tip": ct, "blocks_per_sec": round(rate, 1),
                "pct_to_tip": round(100.0 * h / ct, 3) if ct else None}
        if match and not diverged:
            last_good = h
            base["status"] = "OK"
            base["last_good"] = h
            write_status(base)
        elif not match and not diverged:
            bad = first_divergence(last_good, h)
            rb = compare(bad)
            diff = rb[1] if rb else r[1]
            sentinel = {"status": "DIVERGENCE", "first_bad_block": bad, "last_good": bad - 1,
                        "diff": diff, "head": h, "canonical_tip": ct,
                        "start_block": START_BLOCK, "start_verified": start_verified}
            write_status(sentinel)
            try:
                with open(SENTINEL, "w") as f:
                    f.write(json.dumps(sentinel, indent=2))
            except Exception:
                pass
            diverged = True
        else:
            base["status"] = "DIVERGED_HALTED"
            write_status(base)
            time.sleep(POLL)
            continue
        if time.time() - w_t0 > 900:
            w_h0, w_t0 = h, time.time()
        time.sleep(POLL)


if __name__ == "__main__":
    main()
PYEOF

cat >/etc/systemd/system/arbreth-monitor.service <<EOF
[Unit]
Description=arbreth divergence monitor
After=docker.service
[Service]
Environment=NODE_NAME=${NODE}
Environment=ALCHEMY_KEY=${ALCHEMY_KEY}
Environment=STATUS_BUCKET=${STATUS_BUCKET}
Environment=SNAPSHOT_BLOCK=${SNAPSHOT}
ExecStart=/usr/bin/python3 /opt/arbreth/monitor.py
Restart=always
RestartSec=10
[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
systemctl enable --now arbreth-monitor.service

# ---- 7. periodic stack restart (keeps sync at peak throughput) ----
cat >/etc/systemd/system/arbreth-restart.service <<'EOF'
[Unit]
Description=Periodic arbreth stack restart (sync throughput)
[Service]
Type=oneshot
ExecStart=/usr/bin/bash -c 'cd /opt/arbreth && docker compose restart --timeout 90 || true'
EOF
cat >/etc/systemd/system/arbreth-restart.timer <<'EOF'
[Unit]
Description=Restart arbreth every 15 min
[Timer]
OnBootSec=15min
OnUnitActiveSec=15min
AccuracySec=30s
[Install]
WantedBy=timers.target
EOF
systemctl daemon-reload
systemctl enable --now arbreth-restart.timer

touch "$MARK"
echo "===== provision complete @ $(date -u) ====="
