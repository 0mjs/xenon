#!/bin/zsh
# Start fleet services (all of them when none are named), seeding on first start.
set -euo pipefail
here=${0:A:h}
python3 "$here/../seed/generate.py" --size "${XENON_SEED_SIZE:-small}"
docker compose -f "$here/compose.yml" up -d --wait "$@"
docker compose -f "$here/compose.yml" ps --format 'table {{.Service}}\t{{.Status}}\t{{.Ports}}'
