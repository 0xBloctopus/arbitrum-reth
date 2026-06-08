#!/usr/bin/env bash
# Installed on each fleet VM (run as root) — restarts the arbreth stack every
# 15 min to keep sync at peak throughput. Graceful 90s stop so MDBX flushes.
set -e
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
echo "installed on $(hostname); next fire:"
systemctl list-timers arbreth-restart.timer --no-pager | sed -n '2p'
