# Safety

Xenon is built to be pointed at databases that matter. This page says what it does to protect
them, how to set it up for production, and what is still missing before we call it
production-safe without caveats. It describes what the code does today, not what we hope it does.

**Short version:** with a **read-only database role**, Xenon is safe to look at production.
Writing to production through Xenon is guarded (typed confirmation, server-enforced read-only
reads, all-or-nothing scripts, transactional edits with exact conflict checks), and every guard
below names the automated test that proves it. What isn't proven, or can't be, is listed as a gap.

How this page is kept honest: Xenon is audited at every minor version (most recently 0.5.1, in
seven parts: write detection, editing, SQL text, connections and secrets, robustness, the
workspace, and the supply chain), each finding is proven with a test before it's fixed, and the
fix lands with a test that fails without it. CI runs them on every push
(`.github/workflows/ci.yml`): the drivers and editing against real Postgres, MySQL, MariaDB and
SQL Server, SQLite and DuckDB everywhere, and the app's guards in headless tests on macOS
(`xenon_ui` `snapshots` and `hardening_tests`).

## Set up a production connection

1. **Connect as a role that can only do what you need.** For looking, use a read-only role
   (Postgres: `pg_read_all_data`; MySQL: `SELECT` grants only; SQL Server: `db_datareader`).
   This is the only guarantee no client can take away, and Xenon can't make it for you.
2. **Mark the connection Production** in its settings (it applies to an open connection at
   once). Every statement Xenon recognises as a write then asks first, and you type the
   connection's name to go ahead. So does a script that starts or manages its own transaction.
   Its tabs are underlined in red so you know where you are.
3. **Tick Read-only** when you only mean to look. Xenon enforces it as well as each engine
   allows (see below), on top of the role.
4. **Use `verify-full` TLS** (or an SSH tunnel through your bastion). Postgres's and MySQL's
   default, `prefer`, encrypts but doesn't check who's on the other end, so something posing as
   the server could get your password. SQL Server checks by default, as Microsoft's own tools do.

## What Xenon guards today

| Guard | How | Proven by |
|---|---|---|
| **Spotting writes** | Deny by default: only statements known to read pass, and only when no data-changing verb, side-effect function or server command appears anywhere in them (`delete`, `drop`, `exec`, `select … into` after a CTE too, `nextval` / `pg_catalog.setval`, `dblink`, `lo_*`, `pg_notify`, `reset master`, `start replica`, `set @@global.x`, `kill`, `backup database`, `dbcc`, `pragma name(value)`, DuckDB settings that write files or fetch extensions…). Every script is read ten ways, as each engine and mode would (a lone carriage return ending a `--` comment, comments that nest or don't, `[names]`, MySQL's `#`, backslash and `ANSI_QUOTES` modes, `/*! … */` run as code), and the worst reading counts. | `xenon_driver` `sql::tests`: `comments_and_quotes_read_as_each_engine_does_cant_hide_a_write`, `statements_that_change_the_server_or_files_are_writes`, `mysql_quoting_tricks_cant_hide_a_write`, `sql_server_batches_without_semicolons_count_every_statement`, `anything_not_known_to_read_counts_as_a_write` |
| **Read-only connections** | Postgres: every transaction read-only, every recognised write refused before it's sent, scripts that end transactions part-way refused, switching back refused, and the session checked after every statement. MySQL / MariaDB: a read-only session, every recognised write refused, switching back refused, checked after every statement. SQLite: the file opened read-only. DuckDB: database files opened read-only, and every recognised write refused (CSV and Parquet files, `copy … to`, profiling output). SQL Server: every recognised write refused. | `xenon_postgres::live_read_only_refuses_writes` (including `set_config` with a built name and a `do` block that commits); `xenon_mysql::live::read_only_refuses_writes_and_schema_changes`; `xenon_mssql::live::read_only_refuses_writes`; `xenon_duckdb::read_only_holds_for_files_and_never_fetches_extensions` |
| **Production asks first** | Every way SQL leaves the editor goes through one guard: a recognised write, or a script managing its own transaction, shows the SQL and needs the connection's name typed. The table row count gets the same guards as the browse (never after a refused browse, never when it would write, read-only on production). Nothing runs while a prompt is open, and shortcuts typed in a dialog don't reach the tab behind it. | `xenon_ui` `snapshots`: production writes, Fetch all and `begin` ask; a refused filter isn't counted. `hardening_tests`: `hardening_shortcuts_in_a_dialog_stay_in_it` |
| **The right connection** | A saved production connection never reuses a development session to the same database. A URL (pasted, launched or found on this Mac) for a saved database opens as that saved connection, with its TLS, certificates and tunnel. Editing an open connection's settings applies at once. History runs a statement only on the connection it ran on. | `hardening_tests`: `hardening_production_never_reuses_a_development_session`, `hardening_a_url_for_a_saved_database_opens_it_as_saved`, `hardening_marking_an_open_connection_production_applies_now`, `hardening_history_runs_only_on_its_own_connection` |
| **Production reads can't write** | On Postgres, MySQL and MariaDB, what looks like a read runs in `start transaction read only`, so a write hidden in it (a function that writes) is refused by the server. | `xenon_driver::conformance` `read_only_transaction` |
| **Scripts on production are all or nothing** | A script that writes and that any engine would read as several statements runs as one transaction. If any statement fails, or the script is stopped part-way (⌘., the time limit, closing the tab), it's rolled back and Xenon says nothing was kept. On MySQL, which commits schema changes as they run, the prompt says so and the script isn't presented as all-or-nothing. | `xenon_ui` `snapshots`: a failing, a stopped and a "two statements to SQLite" script keep nothing; `xenon_mysql::live::check` (a later statement's error is reported) |
| **One thing at a time per connection** | A second run on a connection that's running a query or saving is refused, so nothing lands inside another's transaction. | `snapshots`: a second tab is turned away; `hardening_nothing_runs_on_a_connection_while_it_saves` |
| **Production time limits** | A production query is stopped after 1 minute, and so is a save of grid edits (rolled back); a statement waits at most 5 seconds for a lock. Both in Settings → Safety. On SQL Server, a session the stop ended takes its transaction with it, and what comes next fails, saying so, instead of running in autocommit. | `xenon_postgres::live_lock_wait_gives_up`, `xenon_mysql::live::lock_wait_gives_up`, `xenon_mssql::live::lock_wait_gives_up_even_after_a_reconnect` and `a_lost_transaction_is_reported_not_continued`; `snapshots` (a 1-second limit); `xenon_edit` `a_save_past_the_time_limit_is_rolled_back` |
| **Open transactions are shown and kept** | After every query Xenon asks the server; MySQL's driver follows what ran, statement by statement (`begin`, `commit`, `set autocommit = 0`, and the commits schema changes make). Postgres never aborts your transaction behind your back (scripts aren't prepared, and the row limit reads on instead of cancelling inside one), and committing one that had already failed says it rolled back. Disconnecting and quitting ask first; saving waits until it's ended. | `xenon_postgres::live_open_transactions_survive_scripts_and_the_row_limit`; `xenon_mysql::live::check`; `sql::tests::mysql_transactions_are_followed_statement_by_statement`; `conformance` `transactions` |
| **Editing data** | Staged, saved in one transaction to the table the rows came from (named in full, so a `USE` or `search_path` change can't redirect it; rows from other SQL in a table tab aren't editable). Each update and delete must touch exactly one row, found by its primary key, and only if the row still holds what you loaded, compared exactly whatever the column's collation (case, accents, trailing spaces). Rows whose key is NULL aren't saved. A refused commit is rolled back; one lost with the connection is reported as unknown, and saving again waits for a reload. Closing, disconnecting or quitting with unsaved changes asks. | `xenon_edit` tests including `saves_go_to_the_table_that_was_loaded`, `a_case_or_spacing_change_elsewhere_stops_the_save`, `a_refused_commit_rolls_the_save_back`, `a_commit_lost_with_the_connection_is_reported_as_unknown`, `an_edit_never_overwrites_someone_elses_change`; `xenon_mysql` / `xenon_mssql` `conflict_checks_see_case_and_spacing`; `hardening_tests`: `hardening_other_sql_in_a_table_tab_isnt_editable`, `hardening_unsaved_changes_are_never_dropped_silently` |
| **Values are written back exactly** | Literals round-trip exactly (quotes, backslashes, emoji, decimals, blobs, NULL). MySQL strings mean the same in either backslash mode, even one changed after connecting. A bound parameter never joins an operator into a comment. Numbers are written as numbers only when plainly numbers; JSON is edited as stored (large numbers, `1.10`, duplicate keys kept); a NULL cell left empty stays NULL; DuckDB reads typed times as it shows them. | `conformance` `round_trip`; `xenon_mysql::live::check` (both modes); `sql::tests::a_bound_value_never_joins_an_operator_into_a_comment`; `value_editor` tests; `hardening_a_null_cell_left_empty_stays_null` |
| **Row limits never undo a write** | A write that returns more rows than the limit reads and drops the rest instead of stopping, which would roll it back. | `conformance` `capped_write` |
| **Cancel stops the server, not the next query** | ⌘. cancels on the server; a row-limit stop waits until it has landed. SQL Server's cancel ends only the session it was meant for. | `conformance` `cancel` and `row_limit` on every engine |
| **A dropped connection never re-runs anything** | A connection idle for a minute is checked before it's used; if it's gone it's opened again with the same settings and guards. A query is never run again by itself, and if a transaction was open, Xenon says it was rolled back. | `conformance` `alive_after_errors`; each driver's lost-connection live test; `snapshots` |
| **Exports** | CSV and TSV keep text that a spreadsheet would run as a formula (`=…`, `+…`, `-…`, `@…`) as text. | `export::tests::csv_and_tsv_dont_pass_formulas_through` |
| **Secrets** | Passwords live in the macOS Keychain, never in Xenon's files, under names that can't be inherited by another data folder or a later connection. A saved password only goes the saved way (its TLS and tunnel). SSH host keys are checked against Xenon's own `known_hosts` (`accept-new`). | `xenon_store::passwords_have_nowhere_to_go_in_the_data_file`, `keychain_names_are_never_reused`; `secrets::keychain_round_trip`; `xenon_ssh::live` |
| **TLS** | `verify-full` checks the server's certificate and name; a private CA and client certificates are supported. `prefer` takes TLS to a remote server (it falls back to none only on this Mac or through a tunnel), SQL Server encrypts the whole session, and the connect dialog warns about any remote server that isn't verified. A pasted URL never weakens the form's TLS; MySQL refuses an unknown `ssl-mode`. | `xenon_postgres::live_verify_full_rejects_self_signed_certificates`, `prefer_takes_tls_to_remote_servers`; `xenon_mssql::live::prefer_encrypts_the_session`, `no_encrypt_setting_means_encrypt_and_verify`; `xenon_mysql::ssl_modes_are_read_strictly`; `hardening_a_pasted_url_keeps_the_forms_tls` |
| **Hostile data and files** | A value nested thousands deep doesn't crash Xenon; text that isn't valid UTF-8 is never mistaken for another value. | `xenon_duckdb::a_deeply_nested_value_doesnt_crash`; `xenon_edit::an_invalid_utf8_key_never_saves_into_another_row` |
| **Nothing phones home** | Xenon talks only to the databases and bastions you connect to, and to GitHub to see whether a newer release is out (at most daily, sending nothing about you; off in Settings → Data); DuckDB never downloads or loads extensions by itself. | `xenon_duckdb::read_only_holds_for_files_and_never_fetches_extensions`; `updates.rs` is the only other networking code |
| **What ships** | Releases are signed with a Developer ID and notarized. DuckDB's library is checked against its published checksum and DuckDB's own signature before it's built in. The app carries the GPLv3 and every third-party licence and notice. | `scripts/verify-duckdb.sh` (every build); `spctl` in `scripts/release-macos.sh` |

## Known gaps

What's still between Xenon and "safe on production, no caveats", most important first.

| # | Gap | Severity | Plan |
|---|---|---|---|
| 1 | **Spotting writes reads the SQL text.** It's strict (deny by default, ten readings), but a function or procedure that writes, called without any recognisable word, isn't seen. Where the server enforces read-only (Postgres, MySQL), that's caught there; on SQL Server, which has none, it isn't. Use a `db_datareader` login for read-only work there. | Medium | Inherent; documented |
| 2 | **`prefer` and `require` don't check the certificate** (Postgres and MySQL's default, as in their own clients): something posing as the server, with TLS, could get the password. Use `verify-full` for anything that matters. | Medium | Documented; warned about in the dialog |
| 3 | **A URL is matched to a saved connection by name.** An IP for a saved host name opens as a new, unguarded connection. | Low | Documented |
| 4 | **Conflict checks skip spatial columns on MySQL and SQL Server** (their stored form doesn't compare reliably). | Low | When it can compare them exactly |
| 5 | **History and remembered parameter values are kept in Xenon's data file**, which other accounts on the Mac can't read (`~/Library` is private) but isn't encrypted. Settings → Data turns history off for production. | Low | 0.5 hardening (Low items) |
| 6 | **Commit and Roll back aren't stopped by the time limit.** Stopping a rollback part-way only holds its locks longer; the lock wait still applies. | By design | Documented |
| 7 | **SQL Server `money`** is exact only below about 900 billion. | Low | 1.0 at the latest |
| 8 | **Results leave the database when you ask them to.** Copying, exporting and the inspector put data on your clipboard or disk, unencrypted. | By design | Documented |
| 9 | **SSH tunnels listen on `127.0.0.1`.** While one is open, any process running as you can use it, as with `ssh -L`. | By design | Documented |

## Reporting a security problem

Please report it privately through the repository's **Security** tab on GitHub
("Report a vulnerability") rather than in a public issue.
