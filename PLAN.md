# Xenon — Prototype Plan

> A fast, beautiful database client. Rust + GPUI. By Carbon Software.

## Vision

Most DB clients are either powerful and cluttered (DBeaver), pretty but closed (TablePlus),
or Electron-heavy (Beekeeper). Xenon aims to be all three of: **native-fast**, **calm and
keyboard-first**, and **open source**.

**Principles**

1. **Instant.** Cold start < 300 ms. Scrolling 1M rows at 120 fps. Never block the UI thread.
2. **Keyboard-first.** Every action reachable from a command palette (`⌘K`). Mouse optional.
3. **Calm UI.** One focused workspace, no toolbars full of icons. Chrome recedes; data leads.
4. **Safe by default.** Credentials in the OS keychain. Destructive queries need confirmation.
   Read-only connections are a first-class toggle.
5. **Local-first.** No account, no telemetry by default. Your data stays on your machine.

**Long-term goal:** feature parity with Beekeeper Studio, including NoSQL engines
(Redis, MongoDB, Cassandra, DynamoDB, SurrealDB), delivered in finite driver tiers.

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

Results are collected in full, capped at 100,000 rows; past the cap Postgres queries are cancelled
server-side and the result is flagged as truncated. Streaming in **batches** so the first rows
render immediately is still to do.

## Milestones

| # | Milestone | Done when |
|---|---|---|
| M0 ✅ | **Skeleton** | Workspace builds; GPUI window opens with sidebar + empty pane; theme tokens in place |
| M1 ✅ | **SQLite end-to-end** | Open a `.db`, see tables, run a query, see results in a virtual grid |
| M2 ✅ | **Postgres** | Connect via form or URL, introspect schema, run + cancel queries, SSL |
| M3 ✅ | **Editor polish** | Highlighting, run-under-cursor, tabs, query history |
| M4 ✅ | **Grid polish** | Content-sized columns (stripes run to the edge), drag to resize, double-click to fit, copy as TSV/CSV/JSON, table browser with sort/filter, Run/Explain toolbar. Richer per-type rendering (JSON, timestamps) moves to M5 |
| M5 ✅ | **Feel** | Command palette, keybindings, light/dark, empty states, error UX, perf pass |
| M6 | **Ship v0.1** | Signed macOS build, README, screenshots, GitHub release |

Start with SQLite (M1) — no server needed, fastest path to a working loop.

## Design direction: Carbon / Steel

Reference mock: `ui-concepts/06-carbon-steel.html` (local throwaway, not committed; this section is the record).

**Character:** lean, minimal, humble. Dense like an instrument, never "SaaS". Neutrals do the
work; the accent only marks focus.

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
| `bg` | `#0f1012` | `#eeeff1` | Title bar, rail, status bar |
| `s1` | `#141518` | `#f6f6f7` | Panels |
| `s2` | `#18191d` | `#fbfbfc` | Editor, inputs |
| `s3` | `#1e2024` | `#e9eaec` | Hover, current line |
| `line` | `#2a2c31` | `#d6d8dc` | Panel borders |
| `line-2` | `#202226` | `#e4e5e8` | Grid lines |
| `text` | `#e4e5e8` | `#17181b` | Primary text, primary button fill |
| `t2` | `#9b9ea6` | `#5a5e66` | Secondary text |
| `t3` | `#646770` | `#93969d` | Labels, line numbers, NULL |
| `acc` (steel) | `#8fa3bd` | `#4b6481` | Focus only: caret, active tab, selection |
| `acc-wash` | `rgba(143,163,189,.11)` | `rgba(75,100,129,.10)` | Selected row / tree item |
| `ok` | `#94b39d` | `#4d7a5a` | Success, strings |
| `warn` | `#c9a96e` | `#8a6822` | READ tag, numbers |
| `err` | `#d0817a` | `#a8473f` | PROD tag, errors, destructive |

Syntax: keywords = `text` (weight 500), functions = `acc`, strings = `ok`, numbers = `warn`,
comments = `t3` italic.

**Type:** Archivo (UI, 500/600; headings 22 px max) + IBM Plex Mono (editor, grid, labels).

**Components:** primary button is solid `text` on `bg` (no colour); secondary buttons are
outlined. Env tags are 1 px outlined mono caps. Errors: left danger border, error code, line/col,
"did you mean" with `Tab` to apply. Destructive statements on PROD need the table name typed.

## UX notes

- **Empty state** is the onboarding: "Open a SQLite file" / "Connect to Postgres" / paste a URL.
- **Errors** appear inline under the statement that failed, with the Postgres error position
  highlighted in the editor.
- **Motion:** minimal and fast (≤ 120 ms); nothing animates while you're typing.

## Performance budgets

| Metric | Target |
|---|---|
| Cold start to interactive | < 300 ms |
| First rows visible after query returns | < 50 ms |
| Grid scroll, 1M rows | 120 fps, no dropped frames |
| Idle memory | < 100 MB |

## Key risks

1. **SQL editor** — GPUI ships no standalone editor (Zed's is welded to Zed), so the basics
   Beekeeper gets free from CodeMirror (editing, undo, selection, IME, highlighting) are on us.
   Parity autocomplete is simple (keywords, tables, columns); smarter is a later bonus.
2. **Data grid** — virtualization, selection, keyboard nav, inline editing, huge cells; all custom.
3. **Type fidelity** — every type must display, edit and round-trip losslessly (numeric, tz,
   intervals, arrays, enums, bytea, geometry). Safe `UPDATE` generation needs reliable keys.
4. **Rust driver gaps** — Oracle, Snowflake, Firebird, SQL Anywhere lack mature native crates.
5. **Connectivity** — SSH tunnels/jump hosts, TLS variants, cloud IAM auth, reconnect after sleep.
6. **GPUI maturity** — pre-1.0, API churn, thin docs, accessibility/IME gaps. Pin versions.

## Open questions

- License: GPLv3 (+ CLA for dual licensing) vs MIT/Apache-2.0
- Platform order: macOS first, then Linux and Windows (GPUI supports all three)
- Name availability: GitHub org/repo, crates.io `xenon`, domain
