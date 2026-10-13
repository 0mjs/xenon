# Xenon

A fast, minimal database client. Rust + GPUI. By Carbon Software.

Early prototype. See [PLAN.md](PLAN.md).

## Run

```sh
cargo run -p xenon                     # empty window
cargo run -p xenon -- path/to/file.db  # open a SQLite database
cargo run -p xenon -- "postgres://user:pass@localhost/db?sslmode=require"
```

In the app: `⌘O` opens a SQLite file (or drop one on the window), `⌘N` connects to Postgres.

Requires macOS with Xcode (for Metal) and Rust 1.85+ (edition 2024).

| Shortcut | Action |
|---|---|
| `⌘O` | Open SQLite file |
| `⌘N` | Connect to Postgres |
| `⌘↵` | Run the selection, or the whole editor |
| `⌘.` | Cancel the running query |
| `⌘B` | Toggle explorer |
| `⌘I` | Toggle inspector |
| `⌘Q` | Quit |

## Tests

```sh
cargo test --workspace
# Postgres integration tests run when a server is given:
XENON_TEST_PG_URL="postgres://postgres:secret@localhost:5432/postgres" cargo test -p xenon_postgres
```

Fonts: Archivo and IBM Plex Mono, bundled under the SIL Open Font License (`assets/fonts`).
