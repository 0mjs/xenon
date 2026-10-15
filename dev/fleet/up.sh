#!/bin/zsh
# Start fleet services (all of them when none are named), seeding on first start.
set -euo pipefail
here=${0:A:h}
# A throwaway key for the bastion (never use it anywhere else).
[[ -f "$here/ssh/id_ed25519" ]] || { mkdir -p "$here/ssh" && ssh-keygen -q -t ed25519 -N '' -C xenon-fleet -f "$here/ssh/id_ed25519"; }
# Throwaway certificates for the TLS tests.
"$here/tls-setup/make.sh"
python3 "$here/../seed/generate.py" --size "${XENON_SEED_SIZE:-small}"
docker compose -f "$here/compose.yml" up -d --wait "$@"
docker compose -f "$here/compose.yml" ps --format 'table {{.Service}}\t{{.Status}}\t{{.Ports}}'

# SQL Server has no init-script folder: load the seed once, the first time it's up.
compose=(docker compose -f "$here/compose.yml")
if "${compose[@]}" ps --services --status running | grep -qx mssql; then
  sqlcmd=("${compose[@]}" exec -T mssql /opt/mssql-tools18/bin/sqlcmd -S localhost -U sa -P Xenon-fleet-1 -C -b)
  if [[ $("${sqlcmd[@]}" -h -1 -W -Q "set nocount on; select count(*) from sys.databases where name = 'shop'") == 0 ]]; then
    echo "seeding mssql…"
    "${sqlcmd[@]}" -f 65001 -i /seed/mssql.sql -o /dev/null
  fi
fi
