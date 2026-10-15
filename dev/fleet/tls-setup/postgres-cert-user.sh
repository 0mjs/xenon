#!/bin/sh
# Fleet init: `xenon_cert` logs in to Postgres with its client certificate only.
set -eu
psql -v ON_ERROR_STOP=1 -U xenon -d shop -c "create user xenon_cert; grant all on schema public to xenon_cert;"
# First match wins: put the certificate rule ahead of the password ones.
sed -i '1i hostssl all xenon_cert all cert' "$PGDATA/pg_hba.conf"
