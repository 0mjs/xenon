#!/bin/zsh
# Stop the fleet. Add -v to also delete the data volumes (they're re-seeded next start).
set -euo pipefail
docker compose -f "${0:A:h}/compose.yml" down "$@"
