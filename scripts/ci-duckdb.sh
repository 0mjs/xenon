#!/usr/bin/env bash
# Fetch DuckDB's prebuilt library outside target/, check the archive against its
# pinned checksum before unzipping it, and point the duckdb crate at it. Keep the
# version in step with the `duckdb` crate (1.MMmmpp → MM.mm.pp: 1.10506 is DuckDB 1.5.6).
#
# In CI it writes the variables to $GITHUB_ENV. Elsewhere (release builds) it prints
# them as exports:  eval "$(scripts/ci-duckdb.sh)"
set -euo pipefail
version=1.5.6
case "$(uname -s)" in
  Darwin) archive=libduckdb-osx-universal.zip ;;
  Linux) archive=libduckdb-linux-amd64.zip ;;
esac
if [[ -n "${RUNNER_TEMP:-}" ]]; then
  dir="$RUNNER_TEMP/duckdb-$version"
else
  dir="$HOME/Library/Caches/xenon/duckdb-$version"
fi
mkdir -p "$dir"
verify="$(dirname "$0")/verify-duckdb.sh"
# A cached archive is checked again; a bad one is fetched afresh.
if [[ ! -f "$dir/$archive" ]] || ! "$verify" --zip "$dir/$archive" 2>/dev/null; then
  curl -fsSL -o "$dir/$archive" "https://github.com/duckdb/duckdb/releases/download/v$version/$archive"
  "$verify" --zip "$dir/$archive"
fi
unzip -oq "$dir/$archive" -d "$dir"
# Cargo only puts library folders inside target/ on the loader path for tests.
vars=(
  "DUCKDB_LIB_DIR=$dir"
  "DUCKDB_INCLUDE_DIR=$dir"
  "LD_LIBRARY_PATH=$dir"
  "DYLD_FALLBACK_LIBRARY_PATH=$dir"
)
if [[ -n "${GITHUB_ENV:-}" ]]; then
  printf '%s\n' "${vars[@]}" >> "$GITHUB_ENV"
else
  printf 'export %q\n' "${vars[@]}"
fi
