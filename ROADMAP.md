# Xenon 1.0 Roadmap

> What "done" looks like for 1.0, and the order we get there. [PLAN.md](PLAN.md) holds the
> design system, architecture and the milestone currently in flight; this file is the map.

## What 1.0 means

A client you can use all day against every database a typical team runs, for both querying
and changing data and schema, on macOS, Linux and Windows. It should be fast enough that you
stop noticing it and calm enough that you'd want it open.

**Scope:** we target **80–90% of what a mature open-source database GUI does**. The 10–20% we
skip is long-tail engines and rarely used admin screens, not core workflows. Every item below is
tagged:

- **1.0**: must ship for 1.0
- **1.x**: wanted, but can follow 1.0
- **Later**: only if people ask; possibly part of a paid tier

**1.0 is done when:**

1. Every **1.0** row in the feature tables is checked off.
2. Every Tier 1 and Tier 2 engine passes the shared conformance suite (see [Testing](#testing))
   against real servers in CI.
3. The performance budgets in PLAN.md hold for every Tier 1 engine.
4. Signed builds ship for macOS (notarized), Windows (signed MSI) and Linux (AppImage, `.deb`,
   Flatpak), with auto-update.
5. There have been no data-loss bugs for 60 days. Every write path previews its SQL and runs
   in a transaction where the engine supports one.

---

## Engines

There are four tiers, and each one is a finite list. Engines that speak the Postgres or MySQL
wire protocol reuse those drivers and only need introspection tweaks, so one driver covers many
products.

| Tier | Engine | Driver | Covers | Target |
|---|---|---|---|---|
| **1** | PostgreSQL ✅ | `tokio-postgres` | Postgres 12+, Neon, Supabase, RDS/Aurora, Cloud SQL, Timescale | 0.1 |
| **1** | SQLite ✅ | `rusqlite` | Any `.db` file | 0.1 |
| **1** | MySQL / MariaDB | `mysql_async` | MySQL 5.7–9, MariaDB, TiDB, PlanetScale, Vitess | 0.2 |
| **1** | SQL Server | `tiberius` | SQL Server 2017+, Azure SQL | 0.2 |
| **2** | DuckDB | `duckdb` (bundled) | DuckDB files; CSV / Parquet / JSON opened as tables | 0.2 |
| **2** | CockroachDB | Postgres driver | Cockroach, YugabyteDB | 0.6 |
| **2** | Amazon Redshift | Postgres driver | Redshift (old Postgres dialect, no `RETURNING`) | 0.6 |
| **2** | libSQL / Turso | `libsql` | Remote libSQL, embedded replicas | 0.6 |
| **2** | ClickHouse | `clickhouse` (HTTP) | ClickHouse, ClickHouse Cloud | 0.6 |
| **2** | Oracle | `oracle` (ODPI-C, needs Instant Client) | Oracle 12c+, Autonomous DB | 0.6 |
| **3** | MongoDB | `mongodb` | MongoDB 5+, Atlas, DocumentDB | 0.7 |
| **3** | Redis | `redis` | Redis 6+, Valkey, Upstash, ElastiCache | 0.7 |
| **3** | DynamoDB | `aws-sdk-dynamodb` | DynamoDB, local DynamoDB | 0.7 |
| **3** | Cassandra | `scylla` | Cassandra 4+, ScyllaDB, Astra | 0.7 |
| **4** | BigQuery | REST (`gcp-bigquery-client`) | BigQuery | 1.x |
| **4** | Snowflake | REST (SQL API) | Snowflake | 1.x |
| **4** | Trino / Presto | HTTP client | Trino, Presto, Athena-compatible | 1.x |
| **4** | SurrealDB | `surrealdb` | SurrealDB 1.x/2.x | 1.x |
| **4** | Convex | HTTP, using a deploy key and the admin functions the Convex CLI uses | Convex Cloud, self-hosted Convex | 1.x (experimental) |
| **4** | Firebird | `rsfbclient` | Firebird 3+ | Later |
| **4** | SQL Anywhere, others | ODBC bridge | Anything with an ODBC driver | Later |

Tier 1 engines get everything in this roadmap. Tier 2 engines get everything their engine
supports. Tier 3 engines get a different UI built for their data model (see
[Non-relational](#non-relational-tier-3)). Tier 4 engines are read-mostly at first: browse,
query and export.

**Convex** has no SQL and no wire protocol: data is reached through deployed TypeScript functions.
Xenon would browse tables and documents (reusing the MongoDB document UI) through the same
admin-key endpoints the Convex CLI and dashboard use. Those aren't a stable public API, so it
stays experimental and read-mostly until Convex documents one. Self-hosted Convex runs in Docker,
so it can join the test fleet.

---

## Features

### Connecting

| Feature | Target |
|---|---|
| ✅ URL paste, host/port form, all libpq SSL modes, Keychain secrets | 0.1 |
| ✅ Connections home, environments (prod/dev), read-only mode, typed confirmation on prod | 0.2 |
| ✅ Discover local databases (Docker, ports, `.pgpass`, `pg_service.conf`, `.env`) | 0.2 |
| Several connections open at once; tabs bound to a connection | 0.2 |
| SSH tunnels: password, key, agent, `~/.ssh/config` hosts, jump hosts (`ProxyJump`) | 0.2 |
| Client certificates (mTLS), custom CA, `verify-full` for every TLS engine | 0.3 |
| Reconnect after sleep or network loss, keep tabs and unsaved edits | 0.3 |
| Connection colours and folders; import connections from a URL list or other clients' exports | 0.4 |
| Cloud auth: AWS RDS IAM tokens, Azure AD, GCP Cloud SQL IAM | 1.0 |
| Per-connection settings: row limit, statement timeout, auto-commit vs manual transactions | 1.0 |
| Kerberos / LDAP | Later |

### Querying (editor)

| Feature | Target |
|---|---|
| ✅ Highlighting, run statement under cursor / all, cancel, explain, history, tabs, palette | 0.1 |
| Autocomplete from the live schema: tables, columns (alias-aware), functions, keywords | 0.4 |
| Run a selection; run several statements and get one result tab each | 0.4 |
| Query parameters (`:name`, `$1`, `?`) with a fill-in prompt that remembers values | 0.4 |
| Saved queries in folders, per connection or global; open recent | 0.4 |
| Format SQL (per dialect), comment toggle, multi-cursor, find/replace | 0.4 |
| Inline errors with position, plus "did you mean" for unknown columns and tables | 0.4 |
| Visual EXPLAIN plan: tree plus timings, slowest node highlighted | 0.5 |
| Snippets, plus a scratchpad that survives restarts | 1.0 |
| Manual transaction mode: Begin / Commit / Rollback in the toolbar, with a "pending changes" badge | 1.0 |
| AI: write or explain SQL from schema context (opt-in, bring your own key) | Later |

### Working with data (grid)

| Feature | Target |
|---|---|
| ✅ Virtualized grid, keyboard selection, copy TSV/CSV/JSON, sort, WHERE filter, inspector | 0.1 |
| Stream results in batches; lift the 100k cap; "load more" or full fetch to a file | 0.3 |
| **Inline editing:** edit cells, add rows, duplicate, delete; changes staged, SQL previewed, applied in one transaction | 0.3 |
| Value editors per type: JSON tree, long text, dates, enums (dropdown), booleans, bytea (hex/image), arrays | 0.3 |
| Click-through foreign keys: open the referenced row, or see rows that reference this one | 0.3 |
| Filter builder (column / operator / value chips) alongside the raw WHERE field | 0.3 |
| Pagination with exact counts (where cheap) or estimates | 0.3 |
| Column visibility, pinning, reordering; remembered per table | 0.4 |
| Show results as JSON / as records; transpose a single row | 0.4 |
| Aggregates for a selection in the status bar (sum, avg, count, min, max) | 0.4 |
| Geometry/PostGIS preview on a small map | Later |

### Schema (structure)

| Feature | Target |
|---|---|
| ✅ Schema tree: schemas, tables, views, column counts | 0.1 |
| Full tree: columns, indexes, keys, foreign keys, triggers, sequences, functions/procedures, materialized views, enums/types, extensions | 0.5 |
| Table structure view: columns, types, defaults, nullability, comments, indexes, FKs, checks | 0.5 |
| **Table designer:** create or alter tables, columns, indexes and FKs; shows the generated DDL before applying | 0.5 |
| View DDL for any object; copy `CREATE` statements | 0.5 |
| Rename, drop and truncate with previews and typed confirmation on prod | 0.5 |
| Table info: row estimate, size on disk, index sizes, last vacuum/analyze (Postgres) | 0.5 |
| ER diagram for a schema or selected tables; export PNG/SVG | 1.0 |
| Schema diff between two connections (e.g. staging vs prod) producing migration SQL | 1.x |
| Users, roles and grants editor | 1.x |

### Moving data (import, export, backup)

| Feature | Target |
|---|---|
| Export a result or table to CSV, TSV, JSON, NDJSON, Excel, Markdown, SQL `INSERT`s, Parquet | 0.8 |
| Import CSV / Excel / JSON into a new or existing table: column mapping, type inference, preview, batch inserts | 0.8 |
| Copy a table between connections (same or different engines) | 0.8 |
| Backup and restore, wrapping `pg_dump`/`pg_restore`, `mysqldump`, `sqlite3 .backup` with progress | 0.8 |
| Generate fake data for a table (types and FK-aware) | 1.x |

### Workspace and app

| Feature | Target |
|---|---|
| ✅ Command palette, light/dark/system themes, keyboard shortcuts, menus | 0.1 |
| Restore session: open connections, tabs, editor contents, scroll positions | 0.4 |
| Multiple windows; drag a tab out into its own window | 0.9 |
| Customisable keybindings (JSON file + a UI); Vim mode in the editor | 0.9 |
| Settings UI (fonts, sizes, row limit, timeouts, theme) | 0.9 |
| Auto-update (Sparkle on macOS, equivalents elsewhere), release channels | 0.9 |
| Linux and Windows builds at parity | 0.9 |
| Accessibility: VoiceOver/Narrator labels, full keyboard reachability, contrast check | 1.0 |
| IME and non-Latin input in the editor and grid | 1.0 |
| Docs site: getting started, every engine, keyboard reference | 1.0 |
| Encrypted sync of connections and saved queries between machines | Later (paid) |
| Team workspaces: shared connections and queries | Later (paid) |

### Non-relational (Tier 3)

These engines don't pretend to be tables:

- **MongoDB:** database → collection tree; documents shown as an expandable JSON tree or a
  flattened grid; a filter/projection/sort bar using Mongo query syntax; an aggregation
  pipeline editor with per-stage preview; editing documents in place; viewing indexes.
- **Redis:** key browser with pattern search (`SCAN`, never `KEYS`), type-aware viewers
  (string, hash, list, set, zset, stream, JSON), TTL display and edit, a console with
  command autocomplete, and a memory-usage view per key.
- **DynamoDB:** table list; Query/Scan builder (partition and sort key conditions, GSIs/LSIs);
  item editor; capacity and cost shown before running a Scan.
- **Cassandra:** keyspace → table tree, a CQL editor, and a grid that knows partition and
  clustering keys and warns before `ALLOW FILTERING`.

---

## Release train

Each release is shippable on its own. The order puts the most-used workflows first.

| Release | Theme | Headline |
|---|---|---|
| **0.2** | Connect to more | Several connections, MySQL/MariaDB, DuckDB + files, SQL Server, SSH (M9–M12 in PLAN.md) |
| **0.3** | Change data | Streaming results, inline editing with staged changes, value editors, FK navigation, filter builder |
| **0.4** | Write queries faster | Autocomplete, parameters, saved queries, formatting, session restore |
| **0.5** | Understand and change schema | Full object tree, structure view, table designer with DDL preview, visual EXPLAIN |
| **0.6** | Wider SQL | CockroachDB, Redshift, libSQL, ClickHouse, Oracle |
| **0.7** | Beyond SQL | MongoDB, Redis, DynamoDB, Cassandra |
| **0.8** | Move data | Import, export to every format, copy between connections, backup and restore |
| **0.9** | Everywhere | Linux and Windows, auto-update, settings, keybindings, multiple windows |
| **1.0** | Finish | ER diagrams, cloud IAM auth, accessibility, IME, docs site, stability window |

Tier 4 engines and the **1.x** rows follow 1.0, in an order driven by demand.

---

## Testing

We don't need real production databases to test properly. We need **realistic data**,
**every engine version**, and **realistic networks**, and each of those has a cheap source.

### 1. Local fleet (every day, and in CI)

- `dev/fleet/compose.yml`: one Docker Compose file running every engine we support, pinned to
  the versions we promise (e.g. Postgres 12 and 17, MySQL 5.7 and 8.4, MariaDB 11, SQL Server
  2022, ClickHouse, Mongo, Redis, Cassandra, local DynamoDB, Oracle Free).
  `dev/fleet/up.sh shop` starts only what you name.
- **One seed generator for every engine.** The `nexus-sample` generator becomes
  `dev/seed/`. It emits the same realistic schema (patients, orders, events, JSON blobs, enums,
  NULL-heavy columns, unicode, 10M-row fact tables) as SQL for each dialect, as documents for
  Mongo, and as keys for Redis. Sizes are `small` (CI, seconds) and `large` (manual, millions of rows).
- **A torture schema** per engine covers every data type, odd identifiers (`"Order Items"`,
  reserved words, emoji), composite and missing primary keys, huge cells, deep JSON, and tables
  with 500 columns. Correct type round-tripping is checked here.
- **Conformance suite:** `cargo test -p xenon_conformance --features live`. It runs the same
  checks against every running engine: connect, introspect, every type displays and round-trips,
  cancel, errors carry positions, edits apply in one transaction, and read-only mode actually
  blocks writes. GitHub Actions runs it on every PR with service containers.
- **UI snapshots:** `cargo test -p xenon_ui snapshots -- --ignored` renders every screen
  headlessly. These later become golden-image tests that fail on unexpected pixel changes.

### 2. Managed cloud, free tiers (before each release)

Managed services have quirks Docker doesn't: connection poolers, forced TLS, IAM, read
replicas, and odd versions. Most have free tiers, which is enough:

| Service | Tests |
|---|---|
| Neon, Supabase | Postgres behind PgBouncer / Supavisor (no prepared statements in transaction mode), forced TLS |
| PlanetScale or TiDB Serverless | MySQL over TLS, Vitess quirks (no FKs on some plans) |
| Turso | libSQL remote and embedded replicas |
| MongoDB Atlas (M0) | `mongodb+srv://`, TLS, Atlas users |
| Upstash | Redis over TLS, REST-only limits |
| CockroachDB Cloud (Basic) | Cockroach dialect, serverless cold starts |
| Azure SQL (free offer) | SQL Server cloud auth, Azure AD |

These are kept as saved connections in a **test profile**, with credentials in the Keychain and
never in the repo.

### 3. One remote box (for network realism)

One small VPS (for example a Hetzner CX22, a few euros a month) runs the same compose fleet
behind:

- an **SSH bastion** with key and password users and a second hop (tests jump hosts),
- **real TLS certificates** (Let's Encrypt) to test `verify-full`, plus a self-signed CA,
- `tc netem` profiles that add **latency and packet loss** (e.g. 150 ms, 1%) to test cancel,
  reconnect and streaming over bad links,
- **large datasets** (100M+ rows) for scrolling, export and import benchmarks.

Railway or Fly also work for one-off managed instances. For an always-on test box, a VPS is
cheaper and gives us root access for `netem` and the bastion.

### 4. Real users (from 0.3)

A public beta channel with an opt-in crash reporter (no query text, no data), plus a
"Copy diagnostic info" button for bug reports.

---

## Principles that don't change

- Never block the UI thread. Every driver call runs in the background and can be cancelled.
- Every write shows its SQL before it runs, and runs in a transaction where possible.
- Production connections are visually distinct and need typed confirmation for anything that
  can't be undone.
- Credentials live in the OS keychain. No telemetry by default.
- Local-first: no account needed for any 1.0 feature.
