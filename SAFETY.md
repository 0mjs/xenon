# Safety

Xenon is built to be pointed at databases that matter. This page says what it does to protect
them, how to set it up for production, and what is still missing before we call it
production-safe without caveats. It describes what the code does today, not what we hope it does.

**Short version:** with a **read-only database role**, Xenon is safe to use on production
today. Writing to production through Xenon is guarded (typed confirmation, transactional edits,
conflict checks), but several gaps below still matter. Read them before you rely on it.

## Set up a production connection

1. **Connect as a role that can only do what you need.** For looking, use a read-only role
   (Postgres: `pg_read_all_data`; MySQL: `SELECT` grants only; SQL Server: `db_datareader`).
   This is the only guarantee no client can take away, and Xenon can't make it for you.
2. **Mark the connection Production** in its settings. Every statement Xenon recognises as a
   write then asks first, and you type the connection's name to go ahead. Its tabs are
   underlined in red so you know where you are.
3. **Tick Read-only** when you only mean to look. Xenon enforces it as well as each engine allows
   (see below), on top of the role.
4. **Use `verify-full` TLS** (or an SSH tunnel through your bastion). The default, `prefer`,
   encrypts but doesn't check who's on the other end.

## What Xenon guards today

| Guard | How | Notes |
|---|---|---|
| **Read-only connections** | Postgres: every transaction is read-only (`default_transaction_read_only`), and statements that switch it back (`begin read write`, `set … read write`) are refused. MySQL / MariaDB: the session is read-only, schema changes are refused, and so is switching back. SQLite and DuckDB: the file is opened read-only. SQL Server: statements recognised as writes are refused. | SQL Server has no read-only session, so its guard depends on recognising writes (see gaps). The role is the real guarantee everywhere. |
| **Production asks first** | Any statement recognised as a write (insert, update, delete, merge, DDL, grants, `exec`/`call`/`do`, `select … into`, `explain analyze` of a write) shows the SQL and needs the connection's name typed. | Recognition reads the SQL text; see gaps. |
| **Risky statements ask everywhere** | `DROP`, `TRUNCATE`, `ALTER`, and `UPDATE`/`DELETE` with no `WHERE` ask on every connection (Settings → Safety). | On by default. |
| **Editing data** | Changes are staged (amber) and shown as SQL before saving. Saving runs them in **one transaction**: every update and delete must touch **exactly one row**, found by its primary key, and an update only applies if the cells you edited **still hold what you loaded**. Otherwise nothing is saved and it says why. | Tables without a primary key, views and query results can't be edited. Production always shows the review. |
| **Scripts on production are all or nothing** | On production, a script of several statements that writes runs as one transaction: if any statement fails, it's rolled back and Xenon says nothing was kept; when it succeeds, it says it ran as one. Scripts that manage their own transaction (`begin`, `commit`, `savepoint`…) are left alone. | MySQL commits schema changes (`create`, `alter`…) on its own, so a script mixing them isn't atomic there. |
| **Production time limits** | A query on production is stopped after 1 minute, and a statement waits at most 5 seconds for another session's lock (Postgres `lock_timeout`, MySQL `innodb_lock_wait_timeout` / `lock_wait_timeout`, SQL Server `LOCK_TIMEOUT`), so nothing hangs or queues others behind it. | Both in Settings → Safety; the lock wait applies from the next connect. |
| **Open transactions are shown** | After every query Xenon asks the server whether a transaction is open. If one is, the status bar says **transaction open** with Commit and Roll back, and disconnecting asks first. | DuckDB can't say, so it shows nothing; MySQL needs the `PROCESS` privilege to tell (MariaDB doesn't). Quitting doesn't ask yet. |
| **Row limits never undo a write** | When a write returns rows (`delete … returning`, SQL Server's `output`), Xenon reads and drops the rows past the limit rather than stopping the statement, which would roll it back. | Checked by the conformance suite on every engine that can. |
| **Cancel stops the server, not just the screen** | ⌘. cancels on the server (Postgres cancel request, `KILL QUERY`, SQLite/DuckDB interrupt, SQL Server `KILL`). Stopping a query at the row limit waits for the cancel to land, so it can't hit the next query. | SQL Server ends the session to cancel; see gaps. |
| **Secrets** | Passwords and SSH passphrases live in the macOS Keychain, never in Xenon's files. SSH host keys are checked against Xenon's own `known_hosts`; a new key is recorded, a changed one is refused. | |
| **Nothing phones home** | Xenon talks only to the databases and bastions you connect to. No telemetry, no analytics, no update pings. | |
| **Signed and notarized** | Releases are signed with a Developer ID and notarized by Apple. | |
| **Tested against real servers** | One conformance suite (values round-trip exactly, affected rows, row limits, streaming, errors, quoting, rollback, keys, cancel, capped writes) runs against Postgres, MySQL, MariaDB, SQL Server, SQLite and DuckDB on every push. | See `.github/workflows/ci.yml`. |

## Known gaps

What still stands between Xenon and "safe on production, no caveats", most important first.

### High

1. **Recognising writes is a reading of the SQL, not a guarantee.** It misses side effects
   inside reads, such as `select nextval(…)`, `select setval(…)`, `select pg_terminate_backend(…)`,
   `dblink_exec`, or a function that writes. Production may not ask for those, and on SQL Server
   a read-only connection may let them through. *Fix:* a read-only role; and, planned, using
   the server's own answer where it has one (for example running production reads in a
   read-only transaction on Postgres).
2. **Opening a table can be expensive.** Browsing runs `select … limit 1000`, and sorting by a
   column adds `order by`; on a very large table without an index on that column, that's a
   full scan. *Planned:* warn before sorting large tables on unindexed columns (from the
   Structure view's index list).

### Medium

3. **TLS defaults to `prefer`**, which encrypts but doesn't verify the server, so it is open to
   interception on an untrusted network. *Planned:* `verify-full` by default for production
   connections, and a warning when production uses less.
4. **History keeps SQL on disk.** Query history and reopened tabs store SQL text, including any
   values typed into it, in `~/Library/Application Support/Xenon/xenon.db`, protected only by
   your macOS account (and FileVault). *Planned:* an option not to record history for
   production connections. Today: Settings → Data → Clear history.
5. **Quitting with a transaction open doesn't ask.** Disconnecting does; quitting ends the
   session, which rolls the transaction back. *Planned:* ask before quitting too.
6. **Cancelling on SQL Server ends the session.** That also rolls back a transaction you had
   open and loses temporary tables and session settings. *Planned:* say so before cancelling
   when a transaction is open.
7. **Deletes don't check the row is unchanged.** Updates from the grid only apply if the
   edited cells still hold what you loaded; a delete only checks the row still exists.
8. **Results leave the database when you ask them to.** Copying, exporting and the inspector
    put data on your clipboard or disk, unencrypted. That's the point of them, but treat
    exports of production data accordingly.

### Low

9. **SSH tunnels listen on `127.0.0.1`.** While a tunnel is open, any process running as you
    can use it.
10. **Saving edits on real servers isn't in CI yet.** The editing tests run against SQLite in
    CI and against the other engines locally (`dev/fleet`); the driver-level suite runs on all
    of them in CI.
11. **SQL Server `money`** is exact only below about 900 billion (the driver decodes it as a
    floating-point number).

## Checklist to "production-ready"

- [x] Read-only enforced by every engine's session or file mode (and by refusing writes on SQL Server)
- [x] Production confirms every recognised write, with the connection's name typed
- [x] Grid edits: one transaction, exactly one row each, primary key required, no lost updates
- [x] Row limits never roll back a write; a row-limit cancel can't stop the next query
- [x] Secrets in the Keychain; SSH host keys checked
- [x] Conformance suite against every engine in CI
- [x] Statement and lock timeouts, with production defaults
- [x] Open-transaction indicator, with Commit / Roll back, and a prompt before disconnecting
- [ ] A prompt before quitting with a transaction open
- [x] Scripts as one transaction on production; a failed one says nothing was kept
- [ ] `verify-full` by default for production; warn on weaker TLS
- [ ] Use the server's read-only mode for production reads where it has one
- [ ] History off (or cleared on quit) for production connections
- [ ] Warn before sorting large tables on unindexed columns
- [ ] Deletes from the grid check the row is unchanged
- [ ] Editing tests against every engine in CI

## Reporting a security problem

Please report it privately through the repository's **Security** tab on GitHub
("Report a vulnerability") rather than in a public issue.
