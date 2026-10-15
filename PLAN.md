# Xenon — Prototype Plan

> A fast, beautiful database client. Rust + GPUI. By Carbon Software.

## Vision

Most DB clients are either powerful and cluttered, pretty but closed, or slow because they
ship a browser. Xenon aims to be all three of: **native-fast**, **calm and keyboard-first**,
and **open source**.

**Principles**

1. **Instant.** Cold start < 300 ms. Scrolling 1M rows at 120 fps. Never block the UI thread.
2. **Keyboard-first.** Every action reachable from a command palette (`⌘K`). Mouse optional.
3. **Calm UI.** One focused workspace, no toolbars full of icons. Chrome recedes; data leads.
4. **Safe by default.** Credentials in the OS keychain. Destructive queries need confirmation.
   Read-only connections are a first-class toggle.
5. **Local-first.** No account, no telemetry by default. Your data stays on your machine.

**Long-term goal:** a complete everyday database client, including non-relational engines
(MongoDB, Redis, DynamoDB, Cassandra), delivered in finite driver tiers. The full scope and
release order for 1.0 is in [ROADMAP.md](ROADMAP.md).

## Prototype scope (v0.1)

The goal is a daily-drivable client for **PostgreSQL** and **SQLite**.

**In scope**

- Connection manager: add / edit / test / delete; Postgres (host, port, db, user, SSL mode,
  URL paste) and SQLite (file picker, drag-and-drop a `.db` file)
- Schema sidebar: databases → schemas → tables / views / columns / indexes, with fuzzy filter
- SQL editor: syntax highlighting, run statement under cursor (`⌘↵`), run all (`⇧⌘↵`), cancel
- Results grid: virtualized, resizable columns, copy cells/rows (TSV/CSV/JSON), NULL styling,
  type-aware rendering (JSON, timestamps, bytea, arrays)
- Table browser: open a table → paginated grid with sort + simple filter
- Query history (per connection, searchable)
- Tabs: multiple editors/tables per connection
- Command palette (`⌘K`) and light/dark themes

**Out of scope for v0.1** (tracked for later)

- Inline cell editing + generated `UPDATE`s (v0.2)
- SSH tunnels (v0.2)
- Autocomplete from schema (v0.2)
- Import/export, ER diagrams, MySQL/MariaDB, other engines (v0.3+)
- AI query assistant, team sync (later — possible paid tier)

## Architecture

```
xenon/
├── Cargo.toml              # workspace
└── crates/
    ├── xenon/              # binary: app bootstrap, windows, menus
    ├── xenon_ui/           # GPUI views: sidebar, editor, grid, palette, theme
    ├── xenon_core/         # app state, connections, tabs, history (UI-agnostic)
    ├── xenon_driver/       # Driver trait + shared types (Value, Column, ResultSet)
    ├── xenon_postgres/     # Postgres driver
    ├── xenon_sqlite/       # SQLite driver
    └── xenon_store/        # local persistence: saved connections, history, settings
```

**Key decisions**

| Area | Choice | Why |
|---|---|---|
| UI | `gpui` (Zed), via the `gpui-pre =0.3.9` snapshot | GPU-rendered, native-feeling; same snapshot `gpui-component` 0.7 pins. Needs the `font-kit` feature on `gpui-pre-platform` or no text renders |
| Components | `gpui-component` (Longbridge) for the SQL editor only | Its code editor gives tree-sitter SQL highlighting, undo, search and completion hooks. Styled by `assets/themes/steel.json`. The results grid is our own (`uniform_list`) |
| Postgres | `tokio-postgres` + `postgres-native-tls` | Simple-query protocol for exact text values and multi-statement scripts; single statements are also prepared to get column types. One shared Tokio runtime. All libpq `sslmode`s incl. `verify-ca`/`verify-full` |
| SQLite | `rusqlite` (bundled) | Mature, synchronous, run on a background thread |
| Async | Tokio runtime on a background thread, bridged to GPUI's executor | Drivers need Tokio; GPUI owns the main thread |
| SQL parsing | `sqlparser` | Split statements, detect destructive queries |
| Secrets | `keyring` | macOS Keychain / Windows Credential Manager / Secret Service |
| Local store | `crates/xenon_store`: SQLite at `~/Library/Application Support/Xenon/xenon.db` | Query history today (5,000 entries per connection); saved connections next |

**Driver abstraction** (`crates/xenon_driver`)

Drivers are **blocking** and are called from GPUI's background executor; each driver owns any
runtime it needs (Postgres will run Tokio internally). Cancellation is a separate handle so it
can fire while a query holds the connection.

```rust
pub trait Connection: Send + Sync {
    fn name(&self) -> &str;
    fn server_version(&self) -> String;
    fn execute(&self, sql: &str, row_limit: usize) -> Result<QueryResult>; // last statement's outcome
    fn introspect(&self) -> Result<Vec<Schema>>;
    fn cancel_handle(&self) -> Arc<dyn CancelHandle>;
    fn quote_ident(&self, ident: &str) -> String;
}
```

Results stream: drivers report the first 200 rows as a preview and a running count every 2,000
rows, so the grid fills while the rest load. Results are capped at the row limit (100,000 by
default, or none); past the cap the query is cancelled server-side and the result is flagged as
truncated, with **Fetch all** to run it again uncapped. Cancelling keeps the rows already read.

## Milestones

| # | Milestone | Done when |
|---|---|---|
| M0 ✅ | **Skeleton** | Workspace builds; GPUI window opens with sidebar + empty pane; theme tokens in place |
| M1 ✅ | **SQLite end-to-end** | Open a `.db`, see tables, run a query, see results in a virtual grid |
| M2 ✅ | **Postgres** | Connect via form or URL, introspect schema, run + cancel queries, SSL |
| M3 ✅ | **Editor polish** | Highlighting, run-under-cursor, tabs, query history |
| M4 ✅ | **Grid polish** | Content-sized columns (stripes run to the edge), drag to resize, double-click to fit, copy as TSV/CSV/JSON, table browser with sort/filter, Run/Explain toolbar. Richer per-type rendering (JSON, timestamps) moves to M5 |
| M5 ✅ | **Feel** | Command palette, keybindings, light/dark, empty states, error UX, perf pass |
| M6 ✅ | **Ship v0.1** | `Xenon-0.1.0.dmg`, README with screenshots, GitHub release. Signed with a cloud-managed Developer ID certificate and notarized via `xcodebuild` (`scripts/release-macos.sh`, Xcode wrapper in `macos/`) |

Start with SQLite (M1) — no server needed, fastest path to a working loop.

## Design direction: Carbon / Xenon

Reference mock: `ui-concepts/06-carbon-steel.html` (local throwaway, not committed; this section is the record).

**Character:** lean, minimal, humble. Dense like an instrument, never "SaaS". Neutrals do the
work; one steel-blue accent marks focus and the primary action (revised after first use: darker
chrome and more contrast so panels separate).

**Layout** (four panels)

- **Title bar:** tabs (active tab underlined in accent, or in danger red on a PROD connection),
  `⌘K` "Go to anything" on the right.
- **Icon rail** (44 px): switches the explorer between schema, search, history, saved.
- **Explorer** (~220 px): filter, connections with PROD / READ / LOCAL tags, schema tree with
  row counts. `⌘B` toggles.
- **Workspace:** SQL editor on top (Format / Explain / Run), results below with a metrics header
  (status, rows, time, size) and grid / json / csv views.
- **Inspector** (~250 px): selected row's fields; selected column's sum, min/max, nulls and
  distribution. `⌘I` toggles.
- **Status bar:** connection, server version, transaction mode, read/write, cursor position.

**Grid:** 25 px rows, real column borders, subtle zebra, type under each header, NULL italic
and dimmed, numbers right-aligned, focused cell outlined in accent. Columns are sized to their
content and never stretched; stripes and borders continue to the pane's edge.

**Colour tokens**

| Token | Dark | Light | Use |
|---|---|---|---|
| `bg` | `#060607` | `#e6e8ec` | Title bar, rail, status bar |
| `s1` | `#0b0c0e` | `#f4f5f7` | Panels |
| `s2` | `#101114` | `#ffffff` | Editor, inputs |
| `s3` | `#17191d` | `#e3e6eb` | Hover, current line |
| `line` | `#23252b` | `#cfd3da` | Panel borders |
| `line-2` | `#16171b` | `#e4e7eb` | Grid lines |
| `text` | `#eceef2` | `#111318` | Primary text |
| `t2` | `#a2a6af` | `#535a66` | Secondary text |
| `t3` | `#686c76` | `#8c929d` | Labels, line numbers, NULL |
| `acc` (xenon) | `#b9c7dc` | `#4b5d78` | Focus and primary actions: caret, active tab (2px), selection, Run/Connect, rail and table icons. The cold silver of a xenon lamp |
| `acc-wash` | `rgba(185,199,220,.12)` | `rgba(75,93,120,.12)` | Selected row / tree item |
| `ok` | `#8cc49c` | `#3f7d50` | Success, strings |
| `warn` | `#d8b06a` | `#8f6516` | READ tag, numbers |
| `err` | `#e0837a` | `#b2402f` | PROD tag, errors, destructive |
| `on-acc` | `#0b0c0e` | `#ffffff` | Text on accent / danger fills |
| `overlay` | `#111215` | `#ffffff` | Dialogs, palette |
| `field` | `#08090a` | `#ffffff` | Text field fill (slightly inset) |
| `raised` | `#22252b` | `#ffffff` | Chosen segment of a segmented control |
| `scrim` | `rgba(5,6,7,.6)` | `rgba(17,19,24,.18)` | Behind dialogs |
| `shadow` | `rgba(0,0,0,.5)` | `rgba(17,19,24,.18)` | Under floating surfaces |

Syntax: keywords = `text` (weight 500), functions = `acc`, strings = `ok`, numbers = `warn`,
comments = `t3` italic.

**Type:** Archivo (UI, 500/600; headings 22 px max) + IBM Plex Mono (editor, grid, labels).

**Components** (all in `widgets.rs`; nothing styles its own controls):

- **Radii:** 6 px controls, 8 px cards, 12 px overlays. Tags 3 px.
- **Buttons** are 30 px (26 in toolbars): Primary = solid accent, Secondary = `s3` fill + border,
  Ghost = text until hovered, Danger = solid `err`. Disabled = 45% opacity.
- **Text fields** are 30 px (32 in dialogs) with `field` fill; focus = accent border + 3 px
  `acc-wash` ring. Input text is 12 px (13 px for the launcher's paste field, 14 px in the palette).
- **Segmented controls** replace radio rows: `field` track, `raised` chosen segment.
- **Overlays** (dialogs, palette): `overlay` fill, 12 px radius, soft two-layer shadow, a footer
  bar with keyboard hints. Labels above fields are sentence case, 11.5 px, `t2`.
- **Section labels** in panels stay mono caps 10 px `t3`; everything else is sentence case.
- **Env tags:** mono caps on a 12% wash of their colour with a 35% border.
- **Icons:** Lucide, always given an explicit colour (GPUI SVGs don't inherit text colour).
- Errors: tinted danger card with an icon. Destructive statements on PROD need the connection
  name typed.

Design review: `cargo test -p xenon_ui snapshots -- --ignored` renders every screen in both
themes to `target/snapshots/` without opening a window.

## UX notes

- **Empty state** is the onboarding: "Open a SQLite file" / "Connect to Postgres" / paste a URL.
- **Errors** appear inline under the statement that failed, with the Postgres error position
  highlighted in the editor.
- **Motion:** minimal and fast (≤ 120 ms); nothing animates while you're typing.

## Performance budgets

| Metric | Target | v0.1 (M-series Mac, release build) |
|---|---|---|
| Cold start to interactive | < 300 ms | ~215 ms to first frame (Dock icon decoded after it) |
| First rows visible after query returns | < 50 ms | grid built in 0.2 ms for 100,000 rows; 100k-row Postgres join fetched in 158 ms |
| Grid scroll, 1M rows | 120 fps, no dropped frames | results capped at 100k rows; virtualized list, not yet measured with a frame counter |
| Idle memory | < 100 MB | 115 MB (mostly GPUI); 227 MB holding 100k rows |

Measure with `XENON_TRACE=1`.

## Key risks

1. **SQL editor** — GPUI ships no standalone editor (Zed's is welded to Zed), so the basics
   browser-based clients get free from their editor libraries (editing, undo, selection, IME,
   highlighting) are on us.
   Parity autocomplete is simple (keywords, tables, columns); smarter is a later bonus.
2. **Data grid** — virtualization, selection, keyboard nav, inline editing, huge cells; all custom.
3. **Type fidelity** — every type must display, edit and round-trip losslessly (numeric, tz,
   intervals, arrays, enums, bytea, geometry). Safe `UPDATE` generation needs reliable keys.
4. **Rust driver gaps** — Oracle, Snowflake, Firebird, SQL Anywhere lack mature native crates.
5. **Connectivity** — SSH tunnels/jump hosts, TLS variants, cloud IAM auth, reconnect after sleep.
6. **GPUI maturity** — pre-1.0, API churn, thin docs, accessibility/IME gaps. Pin versions.

## v0.2: connect to more, more easily

Two halves: make getting connected effortless, then add the engines most people use after
Postgres and SQLite. Each milestone ships on its own.

| # | Milestone | Done when |
|---|---|---|
| M7 ✅ | **Connections home** | With nothing open, the window is a launcher: saved connections as cards grouped by environment, recent files, and **New connection** with an engine picker. One "paste anything" field takes `postgres://`, `mysql://`, `sqlserver://` URLs, a `DATABASE_URL=` line or a file path and works out the engine. |
| M8 ✅ | **Find my databases** | **Discover** lists databases already on this Mac: Docker containers exposing Postgres/MySQL/SQL Server ports, Homebrew services on default ports, `~/.pgpass` and `pg_service.conf` entries, and `DATABASE_URL`s in a chosen project's `.env` files. One click to connect or save. |
| M9 ✅ | **Several connections at once** | Connections stay open side by side; the explorer lists each one (fold, close); every tab is bound to a connection (its logo, and its name once several are open); ⌘K searches tables across all of them. A tab with no connection is the Connect screen (⇧⌘N, or + in the explorer). |
| M10 ✅ | **MySQL & MariaDB** | `xenon_mysql` on the synchronous `mysql` crate (text protocol, so values arrive exactly as printed): every TLS mode, cancel and row limits via `KILL QUERY`, multi-statement scripts, syntax-error positions from MySQL's "near '…'" messages, read-only that also blocks DDL. Saved connections are now `Target::Server { engine, … }`; the connect dialog, paste field, discovery (Docker env vars, ports, `.env`) and launcher know both engines; MariaDB shows its own logo once connected. Tested live against MySQL 8.4 and MariaDB 11.4 in the test fleet (`dev/fleet`). |
| M11 | **DuckDB, and open CSV / Parquet** | Driver on the bundled `duckdb` crate. Dropping a `.duckdb`, `.parquet`, `.csv` or `.json` file opens it and queries it with SQL straight away. |
| M12 ✅ | **SQL Server, and SSH tunnels** | `xenon_mssql` on `tiberius` with rustls (macOS's native TLS rejects SQL Server's self-signed certificate even when trusted): SQL Server 2017+ and Azure SQL (routing followed), `mssql://`, JDBC and ADO.NET strings, browsing with `TOP`, Explain via `SHOWPLAN_TEXT`, cancel and row limits by `KILL` from a second connection then reconnecting, affected rows via `@@ROWCOUNT`, error lines as positions. `money` is exact only below ~900 billion (tiberius decodes it as f64). Connections take an `address` so tunnels keep the real host for TLS: `verify-full` works through SSH for Postgres and SQL Server. Any server connection can go through an SSH bastion, with the tunnel shown in the status bar. SSH tunnels ✅ run the system `ssh -N -L` (`xenon_ssh`), so config aliases, ProxyJump, agents and hardware keys work unchanged; passwords and key passphrases go through a temporary askpass script, new host keys are recorded in Xenon's own `known_hosts`, and a database the bastion can't reach is explained. Saved tunnels add `ssh_*` columns (migrated in place); the SSH secret is a second Keychain item. Tested live through the fleet's `bastion` service. |

After v0.2, see [ROADMAP.md](ROADMAP.md): 0.3 change data, 0.4 write queries faster, 0.5 schema,
0.6 wider SQL, 0.7 beyond SQL, 0.8 move data, 0.9 everywhere, 1.0 finish.

## Open questions

- License: leaning GPLv3; to sell commercial licenses later, contributions need a CLA so Carbon
  Software keeps the right to relicense
- Platform order: macOS first, then Linux and Windows (GPUI supports all three)
- Name availability: GitHub org/repo, crates.io `xenon`, domain
