# Safety

Xenon is built to be pointed at databases that matter. This page says what it does to protect
them, how to set it up for production, and what is still missing before we call it
production-safe without caveats. It describes what the code does today, not what we hope it does.

**Short version:** with a **read-only database role**, Xenon is safe to use on production.
Writing to production through Xenon is guarded (typed confirmation, server-enforced read-only
reads, all-or-nothing scripts, transactional edits with conflict checks), and every guard below
names the automated test that proves it. What isn't proven yet is listed as a gap.

How this page is kept honest: each claim was checked by an independent audit of the code
(every path that sends SQL, every claim here, and attempts to slip a write past the checks),
the problems it found were fixed, and each guard now has a test that fails if it breaks. CI
runs them on every push (`.github/workflows/ci.yml`): the drivers and editing against real
Postgres, MySQL, MariaDB and SQL Server on Linux, SQLite and DuckDB everywhere, and the app's
guards in a headless test on macOS.

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

| Guard | How | Proven by |
|---|---|---|
| **Spotting writes** | Deny by default: only statements known to read (`select` without `into`, `show`, `explain` without `analyze`, `use`, transaction control…) pass, and only when no data-changing verb or side-effect function (`delete`, `drop`, `exec`, `nextval`, `pg_terminate_backend`, `xp_cmdshell`…) appears anywhere in them. Each script is read as standard SQL and again as MySQL reads it (`#` comments, backslash escapes, `/*! … */` and `/*M! … */` run as code), and the worse reading counts. This drives the production prompt, SQL Server read-only and the wrapping below. | `xenon_driver` `sql::tests`: `mysql_quoting_tricks_cant_hide_a_write`, `sql_server_batches_without_semicolons_count_every_statement`, `anything_not_known_to_read_counts_as_a_write`, `assess_flags_writes_and_destruction` |
| **Read-only connections** | Postgres: every transaction read-only, switching back refused, and after every statement Xenon checks the session is still read-only (however it was switched, it goes straight back, with an error). MySQL / MariaDB: a read-only session, every recognised write refused (so DDL, `prepare … execute`, `call`), switching back refused, and the same check after every statement. SQLite: the file opened read-only. DuckDB: database files opened read-only, and every recognised write refused (so CSV and Parquet files, and `copy … to`, too). SQL Server: every recognised write refused. | `xenon_postgres::live_read_only_refuses_writes` (including `set_config` and a `do` block that builds the statement at run time); `xenon_mysql::live::read_only_refuses_writes_and_schema_changes` (MySQL and MariaDB); `xenon_mssql::live::read_only_refuses_writes`; `xenon_duckdb::read_only_holds_for_files_and_never_fetches_extensions`; `xenon_sqlite::open_rejects_missing_and_non_database_files` |
| **Production asks first** | Every way SQL leaves the editor (Run, Run all, a selection, Explain, Fetch all, history, saved queries, browsing and filters) goes through one guard: a recognised write shows the SQL and needs the connection's name typed. Nothing else runs while a prompt is open, and the statement shown is the one that runs. | `xenon_ui` headless test (`snapshots.rs`): a production write and Fetch all re-running one both ask |
| **Production reads can't write** | On Postgres, MySQL and MariaDB, what looks like a read runs in `start transaction read only`, so a write hidden in it (a function that writes) is refused by the server. | `xenon_driver::conformance` `read_only_transaction` (Postgres, MySQL, MariaDB) |
| **Scripts on production are all or nothing** | A script of several statements that writes runs as one transaction; if any statement fails, it's rolled back and Xenon says nothing was kept. If the transaction can't start, nothing runs. | `xenon_ui` headless test: a failing production script reports nothing kept, and its first change is gone |
| **One query at a time per connection** | A second run on a connection that's busy is refused, so one tab's transaction can't swallow another's statements. | `xenon_ui` headless test: a second tab on a busy connection is turned away |
| **Production time limits** | A production query is stopped after 1 minute, and so is a save of grid edits (rolled back: nothing is kept); except on SQL Server inside a transaction, where stopping would end the session (there it says so instead); and a statement waits at most 5 seconds for a lock. Both in Settings → Safety. | Lock wait: `xenon_postgres::live_lock_wait_gives_up`, `xenon_mysql::live::lock_wait_gives_up` (MySQL and MariaDB), `xenon_mssql::live::lock_wait_gives_up_even_after_a_reconnect`. Time limit: `xenon_ui` headless test (a 1-second limit stops a long production query and says why; a watchdog thread enforces it, whatever the window is doing); saves: `xenon_edit` `a_save_past_the_time_limit_is_rolled_back` and `xenon_edit::live` against Postgres, MySQL, MariaDB and SQL Server (a slow trigger holds the save past the limit) |
| **Open transactions are shown and kept** | After every query Xenon asks the server; MySQL (which has no reliable way to say) and DuckDB are followed through `begin` / `commit` in what ran. With one open: the status bar says so with Commit and Roll back; disconnecting, quitting (⌘Q, the menu or closing the window) and cancelling on SQL Server ask first; saving grid edits waits until it's ended; and on SQL Server the row limit reads on instead of ending the session. | `xenon_driver::conformance` `transactions` (Postgres, MariaDB, SQL Server, SQLite); `sql::tests::transaction_effect_follows_the_last_begin_or_commit`; headless test: the badge and the quit prompt |
| **Editing data** | Staged, saved in one transaction; each update and delete must touch exactly one row, found by its primary key, and only if the row still holds what you loaded (edited cells for an update, every column for a delete; JSON compared by meaning, XML and SQL Server legacy text by their text, floating-point within a millionth). Production always shows the SQL first; elsewhere, Review SQL does. | `xenon_edit` tests (SQLite and DuckDB) including `an_edit_never_overwrites_someone_elses_change` and `a_delete_never_removes_a_row_someone_else_changed`; `a_changed_number_is_noticed_too`; `xenon_edit::live` against Postgres, MySQL, MariaDB and SQL Server, including a changed JSON, XML and float column each stopping a delete |
| **Row limits never undo a write** | A write that returns more rows than the limit (`delete … returning`, SQL Server `output`) reads and drops the rest instead of stopping, which would roll it back. | `xenon_driver::conformance` `capped_write` (Postgres, MariaDB, SQL Server, SQLite, DuckDB) |
| **Cancel stops the server, not the next query** | ⌘. cancels on the server; a row-limit stop waits until it has landed. | `conformance` `cancel` and `row_limit` (20 rounds) on every engine |
| **A dropped connection never re-runs anything** | A connection idle for a minute is checked before it's used, and one whose query fails with anything but the server's own error is checked after; if it's gone (the Mac slept, the network dropped), it's opened again with the same settings, guards and lock wait. A query is never run again by itself: one that failed mid-way says it may not have run, and if a transaction was open, Xenon says it was rolled back and runs nothing that assumed it. | `xenon_driver::conformance` `alive_after_errors` (a refused statement, even inside a failed transaction, isn't a lost connection) on every engine; `xenon_postgres::live_a_lost_connection_is_noticed`, `xenon_mysql::live::lost_connection_mysql` / `_mariadb`, `xenon_mssql::live::a_lost_connection_is_noticed` (each ends its own session from another connection); `xenon_ui` headless test: an idle dropped connection reconnects then runs, one with a transaction open runs nothing, one that breaks mid-query isn't retried |
| **Values are written back exactly** | Values round-trip through Xenon's SQL literals exactly (quotes, backslashes, emoji, decimals, blobs, NULL), whatever Postgres's `standard_conforming_strings` or MySQL's `NO_BACKSLASH_ESCAPES`. | `conformance` `round_trip` on every engine (numbers compared exactly) |
| **Pasted URLs keep their guards** | A pasted or launched URL for a saved database takes that connection's production label and read-only flag. | `xenon_ui` headless test: a URL for the saved production database matches it; another database doesn't |
| **Secrets** | Passwords live in the macOS Keychain, never in Xenon's files; SSH host keys are checked against Xenon's own `known_hosts`, new ones recorded, changed ones refused (`StrictHostKeyChecking=accept-new`). | `xenon_store::passwords_have_nowhere_to_go_in_the_data_file`; `secrets::keychain_round_trip` (macOS CI); `xenon_ssh::live::a_new_host_key_is_recorded_and_a_changed_one_refused` and the tunnel tests (CI, against the fleet's bastion) |
| **TLS that checks who it's talking to** | `verify-full` checks the server's certificate and name (Postgres, MySQL, MariaDB, SQL Server); a private CA can be trusted for a connection, and a client certificate presented (mutual TLS: Postgres, MySQL, MariaDB). A client certificate's key is never written anywhere but a temporary, owner-only file for MySQL's TLS library, deleted with the connection. | `xenon_postgres::live_verify_full_rejects_self_signed_certificates` and `live_mutual_tls` (the fleet's CA passes verify-full; a certificate-only user logs in with it and is refused without it); `xenon_mysql::live::mutual_tls_mysql` / `_mariadb` (`require x509`); `rsa_keys_convert_to_pkcs8_as_openssl_does` |
| **Nothing phones home** | Xenon talks only to the databases and bastions you connect to, and to GitHub to see whether a newer release is out (at most daily, sending nothing about you or your databases; off in Settings → Data); DuckDB is set never to download or load extensions by itself. | `xenon_duckdb::read_only_holds_for_files_and_never_fetches_extensions`; the update check (`updates.rs`) is the only other networking code in Xenon |
| **Signed and notarized** | Releases are signed with a Developer ID and notarized by Apple. | `spctl` check in `scripts/release-macos.sh` (run by hand for each release) |

## Known gaps

What's still between Xenon and "safe on production, no caveats", most important first.

| # | Gap | Severity | Plan |
|---|---|---|---|
| 1 | **Spotting writes on SQL Server reads the SQL text.** Deny by default makes it strict, but SQL Server has no read-only session or transaction, so a read-only SQL Server connection rests on Xenon reading the SQL right. Use a `db_datareader` login for read-only work. | Medium | Inherent to SQL Server; documented |
| 2 | **Conflict checks skip spatial columns on MySQL and SQL Server.** Their stored form doesn't compare reliably, so an edit or delete doesn't check them for changes made meanwhile. Every other type is checked: JSON by meaning, XML and legacy text by their text, floating-point within a millionth. | Low | When those engines can compare them exactly |
| 3 | **Commit and Roll back aren't stopped by the production time limit.** Stopping a rollback part-way would only hold its locks longer, and a commit is quick; the lock wait still applies. | By design | Documented |
| 4 | **History is kept by default.** Settings → Data → Keep history for production turns it off for production connections. | Low | Documented |
| 5 | **`verify-full` is the default only for new production connections.** An existing connection switched to Production shows a warning instead. | Low | Documented |
| 6 | **SQL Server `money`** is exact only below about 900 billion (the driver decodes it as a floating-point number). | Low | 1.0 at the latest |
| 7 | **Results leave the database when you ask them to.** Copying, exporting and the inspector put data on your clipboard or disk, unencrypted. | By design | Documented |
| 8 | **SSH tunnels listen on `127.0.0.1`.** While a tunnel is open, any process running as you can use it, as with `ssh -L`. | By design | Documented |

## Reporting a security problem

Please report it privately through the repository's **Security** tab on GitHub
("Report a vulnerability") rather than in a public issue.
