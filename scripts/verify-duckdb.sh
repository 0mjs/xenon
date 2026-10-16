#!/usr/bin/env bash
# Check DuckDB's prebuilt library before it's built in, signed as Xenon's and shipped:
# it must be exactly the file DuckDB published for the pinned version, and (on macOS)
# signed by DuckDB's own team. Keep these in step with scripts/ci-duckdb.sh and the
# `duckdb` crate's version.
#
#   scripts/verify-duckdb.sh <libduckdb.dylib>   (the universal library, before lipo)
#   scripts/verify-duckdb.sh --zip <archive.zip>  (the downloaded archive)
set -euo pipefail

# DuckDB 1.5.6.
DYLIB_SHA256=75a519cb914ae36ca22cff0995a8a2e8c9a46990b8c0a479514f741a0d93f344
OSX_ZIP_SHA256=e0bc007d9b0094c0970ac1847a8601d10aad07cbd2910ce586ec810f77b638d6
LINUX_ZIP_SHA256=b845005f5132a7d8180057c35e14a7626632258782f871a90861b19c1c03841b
DUCKDB_TEAM=7NCTQWA3HA

sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
fail() { echo "verify-duckdb: $*" >&2; exit 1; }

if [[ "${1:-}" == "--zip" ]]; then
  archive=$2
  case "$(basename "$archive")" in
    libduckdb-osx-universal.zip) want=$OSX_ZIP_SHA256 ;;
    libduckdb-linux-amd64.zip) want=$LINUX_ZIP_SHA256 ;;
    *) fail "no pinned checksum for $(basename "$archive")" ;;
  esac
  [[ "$(sha256 "$archive")" == "$want" ]] || fail "$archive isn't the archive DuckDB published"
  exit 0
fi

dylib=$1
[[ "$(sha256 "$dylib")" == "$DYLIB_SHA256" ]] || fail "$dylib isn't the library DuckDB published"
codesign --verify --strict "$dylib" || fail "$dylib's signature doesn't verify"
team=$(codesign -dv "$dylib" 2>&1 | sed -n 's/^TeamIdentifier=//p')
[[ "$team" == "$DUCKDB_TEAM" ]] || fail "$dylib is signed by team '$team', not DuckDB's"
