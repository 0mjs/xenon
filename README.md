# Xenon

A fast, minimal database client for Postgres, MySQL, MariaDB, SQL Server, SQLite and DuckDB. Native, written in Rust with
[GPUI](https://gpui.rs). By Carbon Software.

![Browsing a table in Xenon](docs/screenshots/browse.png)

- **Native and quick.** First frame in about 0.2 s; 100,000-row results scroll smoothly.
- **Keyboard first, never keyboard only.** `⌘K` jumps to any table or command; every action
  also has a button or menu item.
- **Safe by default.** Saved connections carry a development / production label and an optional
  read-only mode the database enforces. Risky statements ask first; on production you type the
  connection's name to go ahead. Passwords live in the macOS Keychain.
- **Postgres, MySQL, MariaDB, SQL Server (and Azure SQL), SQLite and DuckDB**, plus CSV, Parquet and JSON files opened straight into SQL, with TLS (from `disable`
  to `verify-full`), cancel,
  exact numbers, query plans.
- **Several connections at once**, side by side: every tab says which database it runs on.
- **SSH tunnels** through a bastion, using your own `ssh`: `~/.ssh/config` aliases, `ProxyJump`, the
  agent (1Password, Secretive) and `known_hosts` all just work.

Version 0.1.1, early. See [ROADMAP.md](ROADMAP.md) for the road to 1.0 and [PLAN.md](PLAN.md) for design and architecture.

## Install

Download `Xenon-0.1.1.dmg` from [Releases](https://github.com/0mjs/xenon/releases), open it and
drag Xenon to Applications. It's signed with Carbon Software's Developer ID and notarized by
Apple.

Requires macOS 12 or later.

## Use

With nothing open, Xenon shows **Connect**: paste a `postgres://`, `mysql://` or `mssql://` URL,
an ADO.NET `Server=…;Database=…` string, a
`DATABASE_URL=` line or a database file path and press Enter; pick a database type; reopen a saved
connection; or use one **found on this Mac** (Docker containers, servers on the usual ports,
`~/.pgpass`, `~/.pg_service.conf`, or a project's `.env` files). `⌘O` opens a SQLite or DuckDB database, or a CSV, Parquet or JSON file, `⌘N`
connects to a server. Connections stay open side by side: `⇧⌘N` (or **+** in the explorer) brings
back Connect to open another, and `⇧⌘W` closes the current tab's connection.

![The command palette](docs/screenshots/palette.png)

| Shortcut | Action |
|---|---|
| `⌘K` / `⌘P` | Command palette: jump to a table or run any command |
| `⌘,` | Settings: theme, accent colour, row limits, editor size, safety, history |
| `⌘O` | Open a database file (SQLite, DuckDB) or a CSV / Parquet / JSON file |
| `⌘N` | New server connection (Postgres, MySQL / MariaDB, SQL Server) |
| `⇧⌘N` | Connections: open another database alongside |
| `⇧⌘W` | Disconnect the current tab's database (or the plug button next to it in the explorer) |
| `⌘↵` | Run the statement under the cursor (or the selection) |
| `⇧⌘↵` | Run everything in the editor (or the selection) |
| `⌘E` | Explain the statement under the cursor |
| `⌘.` | Cancel the running query |
| `⌘S` | Save the query (or a table's staged changes) |
| `⇧⌘S` | Export results: CSV, Excel, JSON, SQL inserts, Markdown, TSV |
| `⌘T` / `⌘W` | New / close query tab |
| `⌃Tab` / `⌃⇧Tab` | Next / previous tab (also `⇧⌘]` / `⇧⌘[`) |
| `⌘Y` | Query history (click loads a query, double-click runs it) |
| `⇧⌘E` | Explorer |
| `⌘B` | Toggle side panel |
| `⌘I` | Toggle inspector |

**Results grid:** click a cell, shift-click or `⇧`+arrows to select a block, arrows to move, `⌘A`
for everything. `⌘C` copies as TSV (pastes into spreadsheets), `⇧⌘C` as CSV, `⌥⌘C` as JSON. Click
a header to sort; drag a header's edge to resize, double-click it to fit. `Esc` returns to the
editor.

**Browsing a table** (click it in the explorer) opens it in its own tab with a `WHERE` filter;
sorting there runs on the server, so you see the true top rows.

**Autocomplete** suggests tables after `FROM` / `JOIN`, columns of the tables in your statement
(through their aliases: `o.` lists `orders`' columns), other schemas' tables, functions and
keywords, as you type; `⌃Space` opens it anywhere.

**Editing a table:** double-click a cell (or select it and press `↵`), type, and press `↵`; **Set
NULL** is next to the field. **+ Row** adds a row (empty cells take the column's default), `⌘⌫`
marks the selected rows for deletion. Changes stay staged, tinted in the grid, until you press
`⌘S`: they're saved in one transaction, and if any row changed or vanished since it was loaded,
nothing is saved. **Review SQL** shows exactly what will run; production connections always ask,
with their name typed. Tables need a primary key to be edited.

**Also:** drag the line under the editor to resize it (double-click to reset); when the database
says where a query failed, the cursor jumps there; View → Theme picks System, Light or Dark; double-click
the title bar to zoom.

## Build from source

Needs macOS with Xcode (for Metal) and Rust 1.85+.

```sh
cargo run -p xenon                     # empty window
cargo run -p xenon -- path/to/file.db  # open a SQLite or DuckDB database (or a .csv, .parquet)
cargo run -p xenon -- "postgres://user:pass@localhost/db?sslmode=require"

./scripts/bundle-macos.sh              # local Xenon.app + DMG, signed ad hoc (this Mac only)
./scripts/release-macos.sh             # notarized release DMG in target/xcode
```

`release-macos.sh` archives the Xcode wrapper in `macos/` and lets Xcode sign it with the team's
cloud-managed Developer ID certificate and notarize it, using the account signed in to Xcode.
`XENON_TRACE=1` prints startup and query timings.

## Tests

```sh
cargo test --workspace
```

Integration tests run against a local **test fleet** in Docker: one of each engine, seeded with
the same shop data plus a table of every type and a table that needs quoting (`dev/seed`).

```sh
dev/fleet/up.sh postgres mysql mariadb     # first start pulls images and seeds (~20k rows each)
dev/fleet/up.sh mssql bastion              # SQL Server (about 1 GB, Rosetta on Apple Silicon), SSH bastion
XENON_TEST_PG_URL="postgres://xenon:xenon@127.0.0.1:55432/shop" \
  cargo test --workspace -- --include-ignored
dev/fleet/down.sh                          # stop; add -v to delete the data
```

`XENON_SEED_SIZE=large dev/fleet/up.sh` seeds millions of rows instead. `cargo test -p xenon_ui
snapshots -- --ignored` renders every screen to `target/snapshots/`; with
`XENON_SNAPSHOT_LIVE=1` it also opens a live MariaDB from the fleet.

## License

Xenon is free software under the [GNU General Public License v3.0](LICENSE). Copyright © Carbon
Software Ltd. See [CONTRIBUTING.md](CONTRIBUTING.md) before sending code.

## Credits

Fonts: Archivo and IBM Plex Mono, under the SIL Open Font License (`assets/fonts`). UI built on
[GPUI](https://gpui.rs) and [gpui-component](https://github.com/longbridge/gpui-kit).
