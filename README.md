# Xenon

A fast, minimal database client for Postgres and SQLite. Native, written in Rust with
[GPUI](https://gpui.rs). By Carbon Software.

![Browsing a table in Xenon](docs/screenshots/browse.png)

- **Native and quick.** First frame in about 0.2 s; 100,000-row results scroll smoothly.
- **Keyboard first, never keyboard only.** `⌘K` jumps to any table or command; every action
  also has a button or menu item.
- **Safe by default.** Saved connections carry a development / production label and an optional
  read-only mode the database enforces. Risky statements ask first; on production you type the
  connection's name to go ahead. Passwords live in the macOS Keychain.
- **Postgres and SQLite**, with TLS (every libpq `sslmode`), cancel, exact numbers, query plans.

Version 0.1, early. See [PLAN.md](PLAN.md) for what's next.

## Install

Download `Xenon-0.1.0.dmg` from [Releases](https://github.com/0mjs/xenon/releases), open it and
drag Xenon to Applications. It's signed with Carbon Software's Developer ID and notarized by
Apple.

Requires macOS 12 or later.

## Use

Open a SQLite file with `⌘O` (or drop it on the window), or connect to Postgres with `⌘N`:
paste a `postgres://` URL or fill in the fields, then **Test** and **Connect**. Saved connections
and recent files are listed under **Saved** in the explorer (✎ to edit, ✕ to remove).

![The command palette](docs/screenshots/palette.png)

| Shortcut | Action |
|---|---|
| `⌘K` / `⌘P` | Command palette: jump to a table or run any command |
| `⌘O` | Open SQLite file |
| `⌘N` | Connect to Postgres |
| `⌘↵` | Run the statement under the cursor (or the selection) |
| `⇧⌘↵` | Run everything in the editor (or the selection) |
| `⌘E` | Explain the statement under the cursor |
| `⌘.` | Cancel the running query |
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

**Also:** drag the line under the editor to resize it (double-click to reset); when Postgres says
where a query failed, the cursor jumps there; View → Theme picks System, Light or Dark; double-click
the title bar to zoom.

## Build from source

Needs macOS with Xcode (for Metal) and Rust 1.85+.

```sh
cargo run -p xenon                     # empty window
cargo run -p xenon -- path/to/file.db  # open a SQLite database
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
# Postgres integration tests run when a server is given:
XENON_TEST_PG_URL="postgres://postgres:secret@localhost:5432/postgres" cargo test -p xenon_postgres
```

## License

Xenon is free software under the [GNU General Public License v3.0](LICENSE). Copyright © Carbon
Software Ltd. See [CONTRIBUTING.md](CONTRIBUTING.md) before sending code.

## Credits

Fonts: Archivo and IBM Plex Mono, under the SIL Open Font License (`assets/fonts`). UI built on
[GPUI](https://gpui.rs) and [gpui-component](https://github.com/longbridge/gpui-kit).
