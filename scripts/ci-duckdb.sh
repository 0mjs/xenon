#!/usr/bin/env bash
# CI: fetch DuckDB's prebuilt library outside target/, where the build cache can't
# half-restore it, and point the duckdb crate at it. Keep the version in step with
# the `duckdb` crate (1.MMmmpp → MM.mm.pp: 1.10506 is DuckDB 1.5.6).
set -euo pipefail
version=1.5.6
case "$(uname -s)" in
  Darwin) archive=libduckdb-osx-universal.zip ;;
  Linux) archive=libduckdb-linux-amd64.zip ;;
esac
dir="${RUNNER_TEMP:-/tmp}/duckdb-$version"
mkdir -p "$dir"
curl -fsSL -o "$dir/$archive" "https://github.com/duckdb/duckdb/releases/download/v$version/$archive"
"$(dirname "$0")/verify-duckdb.sh" --zip "$dir/$archive"
unzip -oq "$dir/$archive" -d "$dir"
# Cargo only puts library folders inside target/ on the loader path for tests.
{
  echo "DUCKDB_LIB_DIR=$dir"
  echo "DUCKDB_INCLUDE_DIR=$dir"
  echo "LD_LIBRARY_PATH=$dir"
  echo "DYLD_FALLBACK_LIBRARY_PATH=$dir"
} >> "$GITHUB_ENV"
