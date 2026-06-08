#!/usr/bin/env bash
# Create the full resync fleet (genesis + 7 archive snapshots), staggered.
# Local-SSD per-family quota = 20TB (53 disks); n2/n2d allow only {4,8,16,24}
# disks (4=1.5TB, 8=3TB, 16=6TB, 24=9TB). 3TB lasts to block ~203M, 6TB past tip.
# N2: 5 VMs (160 vCPU, 18TB); N2D: 3 VMs (96 vCPU, 18TB).
# Usage: ./create-fleet.sh            # all nodes
#        ./create-fleet.sh 13984099   # one label only
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# label  snapshot-block  family  num_local_ssd
FLEET=(
  "genesis     genesis     n2   8"
  "13984099    13984099    n2   8"
  "53455712    53455712    n2   8"
  "89962814    89962814    n2   8"
  "124808115   124808115   n2   16"
  "165696725   165696725   n2d  16"
  "202542211   202542211   n2d  16"
  "242809010   242809010   n2d  16"
)

ONLY="${1:-}"
for row in "${FLEET[@]}"; do
  read -r label snap fam nssd <<<"$row"
  [ -n "$ONLY" ] && [ "$ONLY" != "$label" ] && continue
  "$HERE/create-vm.sh" "$label" "$snap" "$fam" "$nssd"
  [ -n "$ONLY" ] || sleep 45   # stagger to spread the Alchemy/L1 burst
done
