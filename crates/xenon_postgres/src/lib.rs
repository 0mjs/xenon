//! Postgres driver, built on `tokio-postgres` with native TLS.
//!
//! Queries run over the simple-query protocol so every value arrives as
//! Postgres's own text output (exact numerics, intervals, arrays, …) and
//! multi-statement scripts work. For a single statement the query is also
//! prepared, which supplies column types for alignment and the type labels.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use futures_util::{StreamExt as _, pin_mut};
use postgres_native_tls::MakeTlsConnector;
use tokio::runtime::Runtime;
use tokio_postgres::error::{ErrorPosition, SqlState};
use tokio_postgres::types::Type;
use tokio_postgres::{CancelToken, Client, Config, SimpleQueryMessage};
use xenon_driver::{
    CancelHandle, Column, ColumnInfo, Connection, Feed, ForeignKey, IndexInfo, ObjectColumn,
    ObjectKind, Progress, QueryError, QueryOutcome, QueryResult, ResultSet, Schema, SchemaObject,
    TableInfo, TlsFiles, Value, referential_action,
};
pub use xenon_driver::{ConnectOptions, SslMode};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

fn wire_mode(ssl: SslMode) -> tokio_postgres::config::SslMode {
    use tokio_postgres::config::SslMode as Wire;
    match ssl {
        SslMode::Disable => Wire::Disable,
        SslMode::Prefer => Wire::Prefer,
        SslMode::Require | SslMode::VerifyCa | SslMode::VerifyFull => Wire::Require,
    }
}

/// `prefer` falls back to no TLS only on this Mac or through an SSH tunnel.
/// Anywhere else, whatever answers could refuse TLS and ask for the password in
/// plain text, so there it takes TLS (still unverified, as `prefer` is).
fn effective_ssl(options: &ConnectOptions) -> SslMode {
    let local = matches!(
        options.host.as_str(),
        "" | "localhost" | "127.0.0.1" | "::1"
    ) || options.host.starts_with('/');
    match options.ssl {
        SslMode::Prefer if !local && options.address.is_none() => SslMode::Require,
        mode => mode,
    }
}

/// Split a `postgres://` URL or libpq `key=value` string into fields, so it
/// can be saved (with the password kept apart, in the Keychain).
pub fn parse_url(url: &str) -> Result<ConnectOptions> {
    let (url, tls) = extract_tls_files(url);
    let (url, ssl) = extract_sslmode(&url);
    let config: Config = url.parse().context("invalid connection string")?;
    let host = match config.get_hosts().first() {
        Some(tokio_postgres::config::Host::Tcp(host)) => host.clone(),
        #[cfg(unix)]
        Some(tokio_postgres::config::Host::Unix(path)) => path.display().to_string(),
        None => String::new(),
    };
    Ok(ConnectOptions {
        host,
        port: config.get_ports().first().copied(),
        database: config.get_dbname().unwrap_or_default().to_string(),
        user: config.get_user().unwrap_or_default().to_string(),
        password: config
            .get_password()
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .unwrap_or_default(),
        ssl: ssl.unwrap_or_default(),
        read_only: false,
        ssh: None,
        address: None,
        lock_timeout: None,
        tls,
    })
}

pub struct PostgresConnection {
    name: String,
    /// `current_schema()` when connected: where unqualified names resolve.
    schema: Option<String>,
    id: String,
    local: bool,
    version: String,
    client: Client,
    cancel: Arc<PgCancel>,
    read_only: bool,
    /// A transaction was open when last asked (`in_transaction`): the row
    /// limit then never cancels a query, which would abort it.
    transaction_open: std::sync::atomic::AtomicBool,
}

struct PgCancel {
    token: CancelToken,
    tls: MakeTlsConnector,
    /// Set when the user cancels (not the row limit), so the rows read so far are kept.
    user: std::sync::atomic::AtomicBool,
}

impl CancelHandle for PgCancel {
    fn cancel(&self) {
        self.user.store(true, std::sync::atomic::Ordering::SeqCst);
        drop(self.stop());
    }
}

impl PgCancel {
    /// Ask the server to stop the running query. The request goes over its own
    /// connection; the returned task finishes once the server has taken it.
    fn stop(&self) -> tokio::task::JoinHandle<()> {
        let token = self.token.clone();
        let tls = self.tls.clone();
        runtime().spawn(async move {
            // The server doesn't report whether a cancel landed; nothing to do on failure.
            let _ = token.cancel_query(tls).await;
        })
    }
}

/// Past the row limit, rows read and dropped (or time spent) before the query is
/// cancelled: most queries just over the limit finish on their own.
const LIMIT_GRACE_ROWS: usize = 10_000;
const LIMIT_GRACE: Duration = Duration::from_millis(100);
/// Long enough for a cancel the server has taken to reach the session.
const CANCEL_SETTLE: Duration = Duration::from_millis(300);

/// One shared Tokio runtime drives every Postgres connection.
fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("xenon-postgres")
            .enable_all()
            .build()
            .expect("failed to start the Postgres runtime")
    })
}

impl PostgresConnection {
    /// Connect with a `postgres://` URL or a libpq `key=value` string.
    /// Understands every libpq `sslmode`, including `verify-ca` and `verify-full`.
    pub fn connect_url(url: &str) -> Result<Self> {
        let (url, tls) = extract_tls_files(url);
        let (url, ssl) = extract_sslmode(&url);
        let mut config: Config = url.parse().context("invalid connection string")?;
        if let Some(ssl) = ssl {
            config.ssl_mode(wire_mode(ssl));
        }
        let ssl = ssl.unwrap_or(SslMode::Prefer);
        Self::connect(config, ssl, &tls)
    }

    pub fn connect_options(options: &ConnectOptions) -> Result<Self> {
        let mut config = Config::new();
        config
            .host(if options.host.is_empty() {
                "localhost"
            } else {
                &options.host
            })
            // Through a tunnel, its local port; the host stays for TLS (hostaddr below).
            .port(
                options
                    .address
                    .map_or(options.port.unwrap_or(5432), |a| a.port()),
            )
            .user(&if options.user.is_empty() {
                default_user()
            } else {
                options.user.clone()
            })
            .ssl_mode(wire_mode(effective_ssl(options)));
        // `postgres` exists on every server; a database named after the user
        // (libpq's default) often doesn't.
        config.dbname(if options.database.is_empty() {
            "postgres"
        } else {
            &options.database
        });
        if !options.password.is_empty() {
            config.password(&options.password);
        }
        let mut startup = Vec::new();
        if options.read_only {
            startup.push("-c default_transaction_read_only=on".to_string());
        }
        if let Some(wait) = options.lock_timeout {
            startup.push(format!("-c lock_timeout={}", wait.as_millis().max(1)));
        }
        if !startup.is_empty() {
            config.options(startup.join(" "));
        }
        if let Some(address) = options.address {
            // Connect to the tunnel; `host` still names the server for TLS.
            config.hostaddr(address.ip());
        }
        Self::connect(config, options.ssl, &options.tls)
    }

    fn connect(mut config: Config, ssl: SslMode, files: &TlsFiles) -> Result<Self> {
        // Read-only rides in the startup options (set above, or in a URL's `options`).
        let read_only = config
            .get_options()
            .is_some_and(|o| o.contains("default_transaction_read_only=on"));
        if config.get_connect_timeout().is_none() {
            config.connect_timeout(CONNECT_TIMEOUT);
        }
        let tls = tls_connector(ssl, files)?;
        let name = connection_name(&config);
        let id = format!(
            "postgres:{}@{}:{}/{}",
            config.get_user().unwrap_or("postgres"),
            name.rsplit('@').next().unwrap_or("localhost"),
            config.get_ports().first().copied().unwrap_or(5432),
            config
                .get_dbname()
                .or(config.get_user())
                .unwrap_or("postgres"),
        );
        let local = config.get_hosts().iter().all(|host| match host {
            tokio_postgres::config::Host::Tcp(name) => {
                matches!(name.as_str(), "localhost" | "127.0.0.1" | "::1")
            }
            #[cfg(unix)]
            tokio_postgres::config::Host::Unix(_) => true,
        });

        runtime().block_on(async {
            let (client, connection) = config.connect(tls.clone()).await.map_err(describe_error)?;
            runtime().spawn(async move {
                // A dropped connection surfaces as an error on the next query.
                let _ = connection.await;
            });

            let schema = client
                .simple_query("select current_schema()")
                .await
                .map_err(describe_error)?
                .into_iter()
                .find_map(|msg| match msg {
                    SimpleQueryMessage::Row(row) => row.get(0).map(str::to_string),
                    _ => None,
                });

            let version = client
                .simple_query("show server_version")
                .await
                .map_err(describe_error)?
                .into_iter()
                .find_map(|msg| match msg {
                    SimpleQueryMessage::Row(row) => row.get(0).map(str::to_string),
                    _ => None,
                })
                .unwrap_or_default();

            Ok(Self {
                read_only,
                transaction_open: Default::default(),
                name,
                id,
                local,
                schema,
                version: format!(
                    "postgres {}",
                    version.split_whitespace().next().unwrap_or("")
                ),
                cancel: Arc::new(PgCancel {
                    token: client.cancel_token(),
                    tls,
                    user: Default::default(),
                }),
                client,
            })
        })
    }

    async fn run(
        &self,
        sql: &str,
        row_limit: usize,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<QueryResult> {
        let started = Instant::now();
        self.cancel
            .user
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let mut feed = Feed::new(progress);
        // Committing a transaction a failed statement aborted only rolls it back:
        // say so, rather than "committed".
        let statement = sql.trim().trim_end_matches(';').trim();
        if ["commit", "end", "commit work", "commit transaction"]
            .iter()
            .any(|c| statement.eq_ignore_ascii_case(c))
            && let Err(err) = self.client.simple_query("select 1").await
            && err.code() == Some(&SqlState::IN_FAILED_SQL_TRANSACTION)
        {
            let _ = self.client.simple_query("rollback").await;
            self.transaction_open
                .store(false, std::sync::atomic::Ordering::SeqCst);
            return Err(anyhow!(
                "The transaction had already failed, so nothing was committed: it was rolled back."
            ));
        }
        // Prepare only to learn column types. A script of several statements
        // can't be prepared, and the failed attempt would abort an open
        // transaction: only single statements are.
        let types: Option<Vec<Type>> = if xenon_driver::sql::several_statements(sql) {
            None
        } else {
            self.client
                .prepare(sql)
                .await
                .ok()
                .map(|stmt| stmt.columns().iter().map(|c| c.type_().clone()).collect())
        };
        // Inside a transaction a cancel aborts it: the row limit reads and drops
        // the extra rows instead.
        let in_transaction = self
            .transaction_open
            .load(std::sync::atomic::Ordering::SeqCst)
            || xenon_driver::sql::controls_transaction(sql);

        let stream = self
            .client
            .simple_query_raw(sql)
            .await
            .map_err(describe_error)?;
        pin_mut!(stream);

        let mut statements = 0;
        let mut outcome = None;
        let mut earlier = Vec::new();
        let mut current: Option<ResultSet> = None;
        let mut cancelled_for_limit = false;
        let mut limit_stop = None;
        // Past the row limit: when, and how many rows were dropped since.
        let mut over_limit: Option<(Instant, usize)> = None;
        // The server confirmed the row limit's cancel stopped this query.
        let mut cancel_landed = false;
        let writes = xenon_driver::sql::writes(sql);
        let mut cancelled = false;

        while let Some(message) = stream.next().await {
            let message = match message {
                Ok(message) => message,
                Err(err)
                    if cancelled_for_limit && err.code() == Some(&SqlState::QUERY_CANCELED) =>
                {
                    cancel_landed = true;
                    break;
                }
                // Stopped by the user while reading rows: keep what arrived.
                Err(err)
                    if err.code() == Some(&SqlState::QUERY_CANCELED)
                        && self.cancel.user.load(std::sync::atomic::Ordering::SeqCst)
                        && current.as_ref().is_some_and(|r| !r.rows.is_empty()) =>
                {
                    cancelled = true;
                    if let Some(rows) = &mut current {
                        rows.truncated = true;
                    }
                    break;
                }
                Err(err) => return Err(describe_error(err)),
            };
            match message {
                SimpleQueryMessage::RowDescription(columns) => {
                    feed.next_set();
                    current = Some(ResultSet {
                        columns: columns
                            .iter()
                            .map(|c| Column {
                                name: c.name().to_string(),
                                type_name: None,
                            })
                            .collect(),
                        ..Default::default()
                    });
                }
                SimpleQueryMessage::Row(row) => {
                    let Some(result) = &mut current else { continue };
                    if result.rows.len() == row_limit {
                        result.truncated = true;
                        // Read on briefly first: a query about to finish needs no
                        // cancel (and one that lands late would stop the next
                        // query). A write is never stopped: a cancel would roll it
                        // back, so its extra rows are read and dropped instead.
                        let (since, dropped) = over_limit.get_or_insert((Instant::now(), 0));
                        *dropped += 1;
                        if !writes
                            && !in_transaction
                            && limit_stop.is_none()
                            && (*dropped >= LIMIT_GRACE_ROWS || since.elapsed() >= LIMIT_GRACE)
                        {
                            cancelled_for_limit = true;
                            limit_stop = Some(self.cancel.stop());
                        }
                        continue;
                    }
                    result.rows.push(
                        (0..row.len())
                            .map(|ix| row.get(ix).map_or(Value::Null, |v| Value::Text(v.into())))
                            .collect(),
                    );
                    feed.read(result, |preview| {
                        if let Some(types) =
                            types.as_ref().filter(|t| t.len() == preview.columns.len())
                        {
                            apply_types(preview, types);
                        }
                    });
                }
                SimpleQueryMessage::CommandComplete(count) => {
                    statements += 1;
                    earlier.extend(outcome.replace(match current.take() {
                        Some(rows) => QueryOutcome::Rows(rows),
                        None => QueryOutcome::Affected(count),
                    }));
                }
                _ => {}
            }
        }
        // A row-limit cancel must not reach the next query. Postgres applies a
        // cancel a moment after taking it, and ignores one that finds the session
        // idle: so when the query finished before the cancel stopped it, wait for
        // the cancel to land on the idle session before running anything else.
        if let Some(stop) = limit_stop {
            let _ = stop.await;
            if !cancel_landed {
                tokio::time::sleep(CANCEL_SETTLE).await;
            }
        }
        if let Some(rows) = current.take() {
            // Cut short by the row limit: no CommandComplete arrives.
            statements += 1;
            earlier.extend(outcome.replace(QueryOutcome::Rows(rows)));
        }

        let mut outcome = outcome.ok_or_else(|| anyhow!("nothing to run"))?;
        if let (QueryOutcome::Rows(rows), Some(types), 1) = (&mut outcome, &types, statements)
            && types.len() == rows.columns.len()
        {
            apply_types(rows, types);
        }

        Ok(QueryResult {
            outcome,
            earlier,
            statements,
            elapsed: started.elapsed(),
            cancelled,
        })
    }
}

impl Connection for PostgresConnection {
    fn name(&self) -> &str {
        &self.name
    }

    fn id(&self) -> String {
        self.id.clone()
    }

    fn server_version(&self) -> String {
        self.version.clone()
    }

    fn execute_streaming(
        &self,
        sql: &str,
        row_limit: usize,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<QueryResult> {
        if self.read_only && xenon_driver::sql::lifts_read_only(sql) {
            return Err(QueryError {
                message:
                    "This is a read-only connection, so switching it to read-write isn't allowed."
                        .into(),
                position: None,
            }
            .into());
        }
        let result = runtime().block_on(self.run(sql, row_limit, progress));
        if self.read_only {
            // However it was done (a `do` block building the statement, say),
            // a read-only session switched to read-write goes straight back.
            let still = runtime()
                .block_on(
                    self.client
                        .simple_query("show default_transaction_read_only"),
                )
                .ok()
                .and_then(|messages| {
                    messages.into_iter().find_map(|m| match m {
                        SimpleQueryMessage::Row(row) => row.get(0).map(|v| v == "on"),
                        _ => None,
                    })
                });
            if still == Some(false) {
                let _ = runtime().block_on(
                    self.client
                        .simple_query("set default_transaction_read_only = on"),
                );
                return Err(QueryError {
                    message: "This read-only connection was switched to read-write; Xenon \
                              switched it back."
                        .into(),
                    position: None,
                }
                .into());
            }
        }
        result
    }

    fn introspect(&self) -> Result<Vec<Schema>> {
        const QUERY: &str = "
            select n.nspname, c.relname, c.relkind::text,
                   a.attname, format_type(a.atttypid, a.atttypmod), a.attnotnull,
                   coalesce(i.indisprimary, false)
            from pg_class c
            join pg_namespace n on n.oid = c.relnamespace
            left join pg_attribute a
                   on a.attrelid = c.oid and a.attnum > 0 and not a.attisdropped
            left join pg_index i
                   on i.indrelid = c.oid and i.indisprimary and a.attnum = any(i.indkey)
            where c.relkind in ('r', 'p', 'v', 'm', 'f')
              and n.nspname not in ('pg_catalog', 'information_schema')
              and n.nspname not like 'pg\\_toast%'
              and n.nspname not like 'pg\\_temp%'
            order by n.nspname = 'public' desc, n.nspname, c.relname, a.attnum";

        let rows = runtime()
            .block_on(self.client.query(QUERY, &[]))
            .map_err(describe_error)?;

        let mut schemas: Vec<Schema> = Vec::new();
        for row in rows {
            let schema: String = row.get(0);
            let table: String = row.get(1);
            if schemas.last().is_none_or(|s| s.name != schema) {
                schemas.push(Schema {
                    name: schema,
                    objects: Vec::new(),
                });
            }
            let objects = &mut schemas.last_mut().expect("pushed above").objects;
            if objects.last().is_none_or(|o| o.name != table) {
                let kind: String = row.get(2);
                objects.push(SchemaObject {
                    name: table,
                    kind: if kind == "v" || kind == "m" {
                        ObjectKind::View
                    } else {
                        ObjectKind::Table
                    },
                    columns: Vec::new(),
                });
            }
            if let Some(column) = row.get::<_, Option<String>>(3) {
                objects
                    .last_mut()
                    .expect("pushed above")
                    .columns
                    .push(ObjectColumn {
                        name: column,
                        type_name: row.get(4),
                        not_null: row.get(5),
                        primary_key: row.get(6),
                    });
            }
        }
        Ok(schemas)
    }

    fn describe(&self, schema: &str, table: &str) -> Result<TableInfo> {
        const COLUMNS: &str = "
            select a.attname::text, format_type(a.atttypid, a.atttypmod), not a.attnotnull,
                   case a.attidentity
                       when 'a' then 'generated always as identity'
                       when 'd' then 'generated by default as identity'
                       else pg_get_expr(d.adbin, d.adrelid) end,
                   exists (select 1 from pg_index i
                           where i.indrelid = c.oid and i.indisprimary and a.attnum = any(i.indkey)),
                   col_description(c.oid, a.attnum),
                   (select array_agg(e.enumlabel::text order by e.enumsortorder)
                    from pg_enum e where e.enumtypid = a.atttypid)
            from pg_class c
            join pg_namespace n on n.oid = c.relnamespace
            join pg_attribute a on a.attrelid = c.oid and a.attnum > 0 and not a.attisdropped
            left join pg_attrdef d on d.adrelid = c.oid and d.adnum = a.attnum
            where n.nspname = $1 and c.relname = $2
            order by a.attnum";
        const INDEXES: &str = "
            select ic.relname::text, i.indisunique, i.indisprimary,
                   array(select pg_get_indexdef(i.indexrelid, k + 1, true)
                         from generate_series(0, i.indnkeyatts - 1) k order by k)
            from pg_index i
            join pg_class c on c.oid = i.indrelid
            join pg_namespace n on n.oid = c.relnamespace
            join pg_class ic on ic.oid = i.indexrelid
            where n.nspname = $1 and c.relname = $2
            order by i.indisprimary desc, ic.relname";
        const KEYS: &str = "
            select con.conname::text, sn.nspname::text, sc.relname::text,
                   array(select a.attname::text from unnest(con.conkey) with ordinality k(num, ord)
                         join pg_attribute a on a.attrelid = con.conrelid and a.attnum = k.num
                         order by k.ord),
                   tn.nspname::text, tc.relname::text,
                   array(select a.attname::text from unnest(con.confkey) with ordinality k(num, ord)
                         join pg_attribute a on a.attrelid = con.confrelid and a.attnum = k.num
                         order by k.ord),
                   con.confdeltype::text, con.confupdtype::text
            from pg_constraint con
            join pg_class sc on sc.oid = con.conrelid
            join pg_namespace sn on sn.oid = sc.relnamespace
            join pg_class tc on tc.oid = con.confrelid
            join pg_namespace tn on tn.oid = tc.relnamespace
            where con.contype = 'f'
              and ((sn.nspname = $1 and sc.relname = $2) or (tn.nspname = $1 and tc.relname = $2))
            order by sn.nspname, sc.relname, con.conname";
        const VIEW: &str = "
            select pg_get_viewdef(c.oid, true)
            from pg_class c join pg_namespace n on n.oid = c.relnamespace
            where n.nspname = $1 and c.relname = $2 and c.relkind in ('v', 'm')";

        let action = |code: &str| {
            referential_action(match code {
                "r" => "restrict",
                "c" => "cascade",
                "n" => "set null",
                "d" => "set default",
                _ => "",
            })
        };
        runtime().block_on(async {
            let params: [&(dyn tokio_postgres::types::ToSql + Sync); 2] = [&schema, &table];
            let mut info = TableInfo::default();
            for row in self
                .client
                .query(COLUMNS, &params)
                .await
                .map_err(describe_error)?
            {
                info.columns.push(ColumnInfo {
                    name: row.get(0),
                    type_name: row.get(1),
                    nullable: row.get(2),
                    default: row.get(3),
                    primary_key: row.get(4),
                    comment: row.get(5),
                    options: row.get::<_, Option<Vec<String>>>(6).unwrap_or_default(),
                });
            }
            for row in self
                .client
                .query(INDEXES, &params)
                .await
                .map_err(describe_error)?
            {
                info.indexes.push(IndexInfo {
                    name: row.get(0),
                    unique: row.get(1),
                    primary: row.get(2),
                    columns: row.get(3),
                });
            }
            for row in self
                .client
                .query(KEYS, &params)
                .await
                .map_err(describe_error)?
            {
                let key = ForeignKey {
                    name: row.get(0),
                    schema: row.get(1),
                    table: row.get(2),
                    columns: row.get(3),
                    ref_schema: row.get(4),
                    ref_table: row.get(5),
                    ref_columns: row.get(6),
                    on_delete: action(row.get(7)),
                    on_update: action(row.get(8)),
                };
                if key.ref_schema == schema && key.ref_table == table {
                    info.referenced_by.push(key.clone());
                }
                if key.schema == schema && key.table == table {
                    info.foreign_keys.push(key);
                }
            }
            info.definition = self
                .client
                .query_opt(VIEW, &params)
                .await
                .map_err(describe_error)?
                .and_then(|row| row.get(0));
            // -1 until the table has been analyzed.
            info.estimated_rows = self
                .client
                .query_opt(
                    "select c.reltuples::bigint from pg_class c
                     join pg_namespace n on n.oid = c.relnamespace
                     where n.nspname = $1 and c.relname = $2",
                    &params,
                )
                .await
                .map_err(describe_error)?
                .and_then(|row| u64::try_from(row.get::<_, i64>(0)).ok());
            Ok(info)
        })
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        self.cancel.clone()
    }

    fn begin_read_only(&self) -> Option<&'static str> {
        Some("start transaction read only")
    }

    fn unchanged(&self, column: &str, type_name: &str, loaded: &Value) -> Option<String> {
        let kind = type_name.to_ascii_lowercase();
        if loaded.is_null() {
            return Some(format!("{column} is null"));
        }
        let literal = self.literal(loaded);
        Some(match kind.as_str() {
            // `json` has no `=`: compare as `jsonb` (by meaning, not spacing).
            "json" => format!("{column}::jsonb = {literal}::jsonb"),
            "xml" | "geometry" | "geography" => format!("{column}::text = {literal}"),
            // `citext`, or a nondeterministic collation, ignores case or accents:
            // compare the characters exactly.
            "text" | "varchar" | "citext" | "name" => {
                format!("{column}::text collate \"C\" = {literal} collate \"C\"")
            }
            _ => return xenon_driver::default_unchanged(self, column, loaded),
        })
    }

    fn alive(&self) -> bool {
        if self.client.is_closed() {
            return false;
        }
        match runtime().block_on(self.client.simple_query("select 1")) {
            Ok(_) => true,
            // The server answered, if only to refuse (inside a failed transaction).
            Err(err) => err.code().is_some(),
        }
    }

    fn in_transaction(&self) -> Option<bool> {
        // `now()` is when the transaction began; outside one, that's this statement.
        match runtime().block_on(
            self.client
                .simple_query("select now() <> statement_timestamp()"),
        ) {
            Ok(messages) => messages.into_iter().find_map(|message| match message {
                SimpleQueryMessage::Row(row) => row.get(0).map(|v| v == "t"),
                _ => None,
            }),
            // "current transaction is aborted": open, and waiting for a rollback.
            Err(err)
                if err.code()
                    == Some(&tokio_postgres::error::SqlState::IN_FAILED_SQL_TRANSACTION) =>
            {
                Some(true)
            }
            Err(_) => None,
        }
        .inspect(|open| {
            self.transaction_open
                .store(*open, std::sync::atomic::Ordering::SeqCst)
        })
    }

    fn is_local(&self) -> bool {
        self.local
    }

    fn default_schema(&self) -> Option<String> {
        self.schema.clone()
    }

    fn literal(&self, value: &Value) -> String {
        match value {
            Value::Blob(bytes) => format!("'\\x{}'::bytea", xenon_driver::hex(bytes)),
            Value::Real(n) if !n.is_finite() => format!(
                "'{}'",
                if n.is_nan() {
                    "NaN".into()
                } else if *n > 0. {
                    "Infinity".to_string()
                } else {
                    "-Infinity".into()
                }
            ),
            other => default_literal(other),
        }
    }
}

/// Swap libpq's `verify-ca` / `verify-full` (which tokio-postgres doesn't parse)
/// for `require`, remembering what was asked for.
fn extract_sslmode(url: &str) -> (String, Option<SslMode>) {
    let mut found = None;
    let mut out = url.to_string();
    for mode in SslMode::ALL {
        let needle = format!("sslmode={}", mode.as_str());
        if let Some(pos) = out.find(&needle) {
            let end = pos + needle.len();
            let boundary = out[end..]
                .chars()
                .next()
                .is_none_or(|c| c == '&' || c.is_whitespace());
            if boundary {
                found = Some(mode);
                if matches!(mode, SslMode::VerifyCa | SslMode::VerifyFull) {
                    out.replace_range(pos..end, "sslmode=require");
                }
            }
        }
    }
    (out, found)
}

/// Pull libpq's `sslrootcert`, `sslcert` and `sslkey` (which tokio-postgres
/// doesn't parse) out of a URL or `key=value` string.
fn extract_tls_files(url: &str) -> (String, TlsFiles) {
    let mut out = url.to_string();
    let mut take = |key: &str| -> String {
        let needle = format!("{key}=");
        let Some(start) = out
            .match_indices(&needle)
            .map(|(i, _)| i)
            .find(|&i| i == 0 || matches!(out.as_bytes()[i - 1], b'?' | b'&' | b' ' | b'\t'))
        else {
            return String::new();
        };
        let value_start = start + needle.len();
        let end = out[value_start..]
            .find(['&', ' ', '\t'])
            .map_or(out.len(), |i| value_start + i);
        let value = out[value_start..end].to_string();
        // Take a separator with it: the one before, or else the one after.
        let (from, to) = if start > 0 && matches!(out.as_bytes()[start - 1], b'&' | b' ' | b'\t') {
            (start - 1, end)
        } else if end < out.len() {
            (start, end + 1)
        } else {
            (start, end)
        };
        out.replace_range(from..to, "");
        if out.ends_with('?') {
            out.pop();
        }
        // URL-encoded paths (`%2F`) as written in a URL.
        value
            .replace("%2F", "/")
            .replace("%2f", "/")
            .replace("%20", " ")
            .replace("%7E", "~")
    };
    let tls = TlsFiles {
        ca: take("sslrootcert"),
        cert: take("sslcert"),
        key: take("sslkey"),
    };
    (out, tls)
}

fn tls_connector(ssl: SslMode, files: &TlsFiles) -> Result<MakeTlsConnector> {
    let mut builder = native_tls::TlsConnector::builder();
    if !files.ca.is_empty() {
        let pem = TlsFiles::read(&files.ca, "CA certificate")?;
        let ca = native_tls::Certificate::from_pem(&pem)
            .context("The CA certificate isn't a PEM certificate")?;
        builder.add_root_certificate(ca);
    }
    if let Some((cert, key)) = files.client()? {
        let identity = native_tls::Identity::from_pkcs8(
            &TlsFiles::read(cert, "client certificate")?,
            &xenon_driver::pkcs8_key(&TlsFiles::read(key, "client key")?)?,
        )
        .context("The client certificate and key don't load as PEM")?;
        builder.identity(identity);
    }
    match ssl {
        SslMode::Disable | SslMode::Prefer | SslMode::Require => {
            builder
                .danger_accept_invalid_certs(true)
                .danger_accept_invalid_hostnames(true);
        }
        SslMode::VerifyCa => {
            builder.danger_accept_invalid_hostnames(true);
        }
        SslMode::VerifyFull => {}
    }
    Ok(MakeTlsConnector::new(builder.build()?))
}

fn connection_name(config: &Config) -> String {
    let host = config
        .get_hosts()
        .first()
        .map(|host| match host {
            tokio_postgres::config::Host::Tcp(name) => name.clone(),
            #[cfg(unix)]
            tokio_postgres::config::Host::Unix(path) => path.display().to_string(),
        })
        .unwrap_or_else(|| "localhost".into());
    let database = config
        .get_dbname()
        .or(config.get_user())
        .unwrap_or("postgres");
    format!("{database}@{host}")
}

/// The user name libpq (and psql) assume when none is given: your account name.
/// Homebrew and Postgres.app create a role with it rather than `postgres`.
pub fn default_user() -> String {
    std::env::var("USER")
        .ok()
        .filter(|user| !user.is_empty())
        .unwrap_or_else(|| "postgres".into())
}

fn apply_types(rows: &mut ResultSet, types: &[Type]) {
    for (column, ty) in rows.columns.iter_mut().zip(types) {
        let name = ty.name();
        column.type_name = Some(match name.strip_prefix('_') {
            Some(element) => format!("{element}[]"),
            None => name.to_string(),
        });
    }
    for row in &mut rows.rows {
        for (value, ty) in row.iter_mut().zip(types) {
            if let Value::Text(text) = value {
                *value = typed_value(std::mem::take(text), ty);
            }
        }
    }
}

fn typed_value(text: String, ty: &Type) -> Value {
    match ty.name() {
        "int2" | "int4" | "int8" | "oid" => text
            .parse()
            .map(Value::Integer)
            .unwrap_or(Value::Text(text)),
        "float4" | "float8" => text.parse().map(Value::Real).unwrap_or(Value::Text(text)),
        "numeric" | "money" => Value::Decimal(text),
        "bool" => Value::Text(if text == "t" { "true" } else { "false" }.into()),
        _ => Value::Text(text),
    }
}

/// Turn a driver error into the message a user needs: severity, SQLSTATE,
/// message, and position/detail/hint when the server sent them.
fn describe_error(err: tokio_postgres::Error) -> anyhow::Error {
    let Some(db) = err.as_db_error() else {
        let mut message = err.to_string();
        let mut source = std::error::Error::source(&err);
        while let Some(cause) = source {
            message.push_str(": ");
            message.push_str(&cause.to_string());
            source = cause.source();
        }
        return anyhow!(message);
    };
    let mut message = format!("{} {}: {}", db.severity(), db.code().code(), db.message());
    let position = match db.position() {
        Some(ErrorPosition::Original(position)) => Some(*position as usize),
        _ => None,
    };
    if let Some(position) = position {
        message.push_str(&format!(" (at character {position})"));
    }
    if let Some(detail) = db.detail() {
        message.push_str(&format!("\nDETAIL: {detail}"));
    }
    if let Some(hint) = db.hint() {
        message.push_str(&format!("\nHINT: {hint}"));
    }
    anyhow::Error::new(QueryError { message, position })
}

/// The shared literal rules, for what Postgres doesn't override.
fn default_literal(value: &Value) -> String {
    match value {
        Value::Null => "NULL".into(),
        Value::Integer(n) => n.to_string(),
        Value::Real(n) => n.to_string(),
        Value::Decimal(n) => n.clone(),
        // With a backslash, an escape string: it means the same whatever the
        // server's `standard_conforming_strings`.
        Value::Text(text) if text.contains('\\') => {
            format!("E'{}'", text.replace('\\', "\\\\").replace('\'', "''"))
        }
        Value::Text(text) => format!("'{}'", text.replace('\'', "''")),
        Value::Blob(bytes) => format!("'\\x{}'::bytea", xenon_driver::hex(bytes)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefer_takes_tls_to_remote_servers() {
        let options = |host: &str| ConnectOptions {
            host: host.into(),
            ssl: SslMode::Prefer,
            ..Default::default()
        };
        assert_eq!(effective_ssl(&options("db.example.com")), SslMode::Require);
        assert_eq!(effective_ssl(&options("localhost")), SslMode::Prefer);
        assert_eq!(effective_ssl(&options("")), SslMode::Prefer);
        let tunneled = ConnectOptions {
            address: Some("127.0.0.1:6000".parse().unwrap()),
            ..options("db.internal")
        };
        assert_eq!(effective_ssl(&tunneled), SslMode::Prefer);
    }

    #[test]
    fn extract_sslmode_rewrites_verify_modes() {
        let (url, mode) = extract_sslmode("postgres://u@h/db?sslmode=verify-full");
        assert_eq!(url, "postgres://u@h/db?sslmode=require");
        assert_eq!(mode, Some(SslMode::VerifyFull));

        let (url, mode) = extract_sslmode("host=h sslmode=verify-ca dbname=x");
        assert_eq!(url, "host=h sslmode=require dbname=x");
        assert_eq!(mode, Some(SslMode::VerifyCa));

        let (url, mode) = extract_sslmode("postgres://u@h/db?sslmode=require&x=1");
        assert_eq!(url, "postgres://u@h/db?sslmode=require&x=1");
        assert_eq!(mode, Some(SslMode::Require));

        assert_eq!(extract_sslmode("postgres://h/db").1, None);
    }

    #[test]
    fn certificate_files_come_from_urls_too() {
        let options = parse_url(
            "postgres://me@db.example.com/shop?sslrootcert=%7E/certs/ca.pem&sslmode=verify-full\
             &sslcert=/certs/me.pem&sslkey=/certs/me.key",
        )
        .unwrap();
        assert_eq!(options.ssl, SslMode::VerifyFull);
        assert_eq!(
            options.tls,
            TlsFiles {
                ca: "~/certs/ca.pem".into(),
                cert: "/certs/me.pem".into(),
                key: "/certs/me.key".into(),
            }
        );
        assert_eq!(options.database, "shop");
        // libpq's key=value form.
        let options =
            parse_url("host=db.example.com dbname=shop sslrootcert=/certs/ca.pem user=me").unwrap();
        assert_eq!(options.tls.ca, "/certs/ca.pem");
        assert_eq!(
            (options.user.as_str(), options.database.as_str()),
            ("me", "shop")
        );
        // Only the certificate parameter: the URL still parses.
        assert_eq!(
            parse_url("postgres://db/shop?sslcert=/c.pem&sslkey=/c.key")
                .unwrap()
                .tls
                .cert,
            "/c.pem"
        );
    }

    #[test]
    fn options_from_url() {
        let options =
            parse_url("postgres://me:p%40ss@db.example.com:6543/shop?sslmode=verify-full").unwrap();
        assert_eq!(
            options,
            ConnectOptions {
                host: "db.example.com".into(),
                port: Some(6543),
                database: "shop".into(),
                user: "me".into(),
                password: "p@ss".into(),
                ssl: SslMode::VerifyFull,
                read_only: false,
                ssh: None,
                address: None,
                lock_timeout: None,
                tls: TlsFiles::default(),
            }
        );
        let minimal = parse_url("postgres://localhost").unwrap();
        assert_eq!(
            (minimal.host.as_str(), minimal.port, minimal.ssl),
            ("localhost", Some(5432), SslMode::Prefer)
        );
        assert!(parse_url("not a url ::").is_err());
    }

    #[test]
    fn live_read_only_refuses_writes() {
        let Ok(url) = std::env::var("XENON_TEST_PG_URL") else {
            return;
        };
        let options = ConnectOptions {
            read_only: true,
            ..parse_url(&url).unwrap()
        };
        let db = PostgresConnection::connect_options(&options).unwrap();
        assert!(db.execute("select 1", 10).is_ok());
        let err = db
            .execute("create table xenon_ro_probe (x int)", 10)
            .unwrap_err()
            .to_string();
        assert!(err.contains("25006"), "{err}");
        // And it can't be switched back on from the editor, however it's written.
        for lift in [
            "set default_transaction_read_only = off",
            "begin read write; create table xenon_ro_probe (x int); commit",
            "select set_config('default_transaction_read_only', 'off', false)",
            // Built at run time, so no check of the text can see it: caught after.
            "do $$ begin execute 'set default_transaction_' || 'read_only = off'; end $$",
        ] {
            let err = db.execute(lift, 10).unwrap_err().to_string();
            assert!(err.contains("read-only connection"), "{lift}: {err}");
        }
        let err = db
            .execute("create table xenon_ro_probe (x int)", 10)
            .unwrap_err()
            .to_string();
        assert!(err.contains("25006"), "still read-only: {err}");
    }

    #[test]
    fn live_lock_wait_gives_up() {
        let Ok(url) = std::env::var("XENON_TEST_PG_URL") else {
            return;
        };
        let holder = PostgresConnection::connect_url(&url).unwrap();
        let waiter = PostgresConnection::connect_options(&ConnectOptions {
            lock_timeout: Some(Duration::from_millis(500)),
            ..parse_url(&url).unwrap()
        })
        .unwrap();
        holder
            .execute(
                "drop table if exists xenon_lock_probe; create table xenon_lock_probe (id int primary key); \
                 insert into xenon_lock_probe values (1)",
                1,
            )
            .unwrap();
        holder
            .execute("begin; update xenon_lock_probe set id = 1", 1)
            .unwrap();
        let started = std::time::Instant::now();
        let blocked = waiter.execute("update xenon_lock_probe set id = 1", 1);
        holder
            .execute("rollback; drop table xenon_lock_probe", 1)
            .unwrap();
        let err = blocked
            .expect_err("the second update didn't wait for the lock")
            .to_string();
        assert!(
            err.contains("55P03") || err.contains("lock timeout"),
            "{err}"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn typed_values() {
        assert_eq!(typed_value("42".into(), &Type::INT8), Value::Integer(42));
        assert_eq!(typed_value("1.5".into(), &Type::FLOAT8), Value::Real(1.5));
        assert_eq!(
            typed_value("12345678901234567890.123".into(), &Type::NUMERIC),
            Value::Decimal("12345678901234567890.123".into())
        );
        assert_eq!(
            typed_value("t".into(), &Type::BOOL),
            Value::Text("true".into())
        );
        assert_eq!(
            typed_value("{a,b}".into(), &Type::TEXT_ARRAY),
            Value::Text("{a,b}".into())
        );
    }

    /// Integration tests run against a live server when `XENON_TEST_PG_URL` is set,
    /// e.g. `postgres://postgres:secret@localhost:5432/postgres`.
    fn live() -> Option<PostgresConnection> {
        let url = std::env::var("XENON_TEST_PG_URL").ok()?;
        Some(PostgresConnection::connect_url(&url).expect("connect to XENON_TEST_PG_URL"))
    }

    fn rows(result: QueryResult) -> ResultSet {
        match result.outcome {
            QueryOutcome::Rows(rows) => rows,
            other => panic!("expected rows, got {other:?}"),
        }
    }

    #[test]
    fn live_a_lost_connection_is_noticed() {
        let (Some(db), Some(other)) = (live(), live()) else {
            return;
        };
        assert!(db.alive());
        let pid = rows(db.execute("select pg_backend_pid()", 1).unwrap()).rows[0][0].clone();
        other
            .execute(
                &format!("select pg_terminate_backend({})", db.literal(&pid)),
                1,
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while db.alive() {
            assert!(
                Instant::now() < deadline,
                "still alive after its session was ended"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn live_types_and_labels() {
        let Some(db) = live() else { return };
        let result = rows(
            db.execute(
                "select 1::int8 as a, 2.5::float8 as b, 9876543210.99::numeric as c, \
                 true as d, null::text as e, array['x','y'] as f, interval '90 minutes' as g",
                10,
            )
            .unwrap(),
        );
        let labels: Vec<_> = result
            .columns
            .iter()
            .map(|c| c.type_name.clone().unwrap())
            .collect();
        assert_eq!(
            labels,
            [
                "int8", "float8", "numeric", "bool", "text", "text[]", "interval"
            ]
        );
        let row = &result.rows[0];
        assert_eq!(row[0], Value::Integer(1));
        assert_eq!(row[1], Value::Real(2.5));
        assert_eq!(row[2], Value::Decimal("9876543210.99".into()));
        assert_eq!(row[3], Value::Text("true".into()));
        assert_eq!(row[4], Value::Null);
        assert_eq!(row[5], Value::Text("{x,y}".into()));
        assert_eq!(row[6], Value::Text("01:30:00".into()));
    }

    #[test]
    fn live_multi_statement_and_affected() {
        let Some(db) = live() else { return };
        let result = db
            .execute(
                "create temp table t (id int); insert into t select generate_series(1, 3); select count(*) from t",
                10,
            )
            .unwrap();
        assert_eq!(result.statements, 3);
        assert_eq!(rows(result).rows[0][0], Value::Text("3".into()));
        let result = db.execute("update t set id = id + 1", 10).unwrap();
        assert!(matches!(result.outcome, QueryOutcome::Affected(3)));
    }

    #[test]
    fn live_row_limit_stops_the_query() {
        let Some(db) = live() else { return };
        let started = Instant::now();
        let result = rows(
            db.execute("select g from generate_series(1, 50000000) g", 1000)
                .unwrap(),
        );
        assert_eq!(result.rows.len(), 1000);
        assert!(result.truncated);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        // The connection is still usable afterwards.
        assert!(db.execute("select 1", 10).is_ok());
    }

    /// Inside the user's transaction, nothing Xenon does aborts it: a script
    /// isn't prepared, and the row limit doesn't cancel. A commit of one a
    /// failed statement aborted says it rolled back.
    #[test]
    fn live_open_transactions_survive_scripts_and_the_row_limit() {
        let Some(db) = live() else { return };
        db.execute("begin", 1).unwrap();
        assert_eq!(db.in_transaction(), Some(true));
        db.execute("select 1; select 2", 10).unwrap();
        assert_eq!(db.in_transaction(), Some(true));
        let big = rows(
            db.execute("select g from generate_series(1, 300000) g", 1000)
                .unwrap(),
        );
        assert!(big.truncated);
        assert!(
            db.execute("select 1", 1).is_ok(),
            "the transaction was aborted"
        );
        db.execute("commit", 1).unwrap();

        db.execute("begin", 1).unwrap();
        assert_eq!(db.in_transaction(), Some(true));
        assert!(db.execute("select 1 / 0", 1).is_err());
        let err = db.execute("commit", 1).unwrap_err().to_string();
        assert!(err.contains("nothing was committed"), "{err}");
        assert_eq!(db.in_transaction(), Some(false));
    }

    #[test]
    fn live_errors_are_descriptive() {
        let Some(db) = live() else { return };
        let err = db
            .execute("select emial from pg_class", 10)
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("ERROR 42703: column \"emial\" does not exist"),
            "{err}"
        );
        assert!(err.contains("at character 8"), "{err}");
        let err = db.execute("select emial from pg_class", 10).unwrap_err();
        assert_eq!(err.downcast_ref::<QueryError>().unwrap().position, Some(8));
    }

    #[test]
    fn live_conformance() {
        let Some(conn) = live() else { return };
        xenon_driver::conformance::run(
            &conn,
            &xenon_driver::conformance::Dialect {
                integer: "integer",
                text: "text",
                decimal: "numeric(12,2)",
                real: "double precision",
                blob: "bytea",
                series: |n| format!("select i from generate_series(1, {n}) as g(i)"),
                slow: "select pg_sleep(5)",
                rollback: "rollback",
                referential_actions: true,
                delete_returning: Some("delete from {table} returning id"),
            },
        );
    }

    #[test]
    fn live_describe() {
        let Some(conn) = live() else { return };
        conn.execute(
            "drop table if exists xenon_lines, xenon_heads;
             drop type if exists xenon_mood;
             create type xenon_mood as enum ('calm', 'busy');
             create table xenon_heads (id bigint generated always as identity primary key, code text unique,
                                       mood xenon_mood);
             comment on column xenon_heads.code is 'Short code';
             create table xenon_lines (
                 head_id bigint not null references xenon_heads on delete set null,
                 n int default 1,
                 primary key (head_id, n)
             );",
            10,
        )
        .unwrap();
        let heads = conn.describe("public", "xenon_heads").unwrap();
        assert_eq!(
            heads.columns[0].default.as_deref(),
            Some("generated always as identity")
        );
        assert_eq!(heads.columns[1].comment.as_deref(), Some("Short code"));
        assert_eq!(heads.columns[2].options, ["calm", "busy"]);
        assert!(heads.indexes[0].primary);
        assert_eq!(heads.referenced_by[0].table, "xenon_lines");
        let lines = conn.describe("public", "xenon_lines").unwrap();
        assert_eq!(lines.foreign_keys[0].single(), Some(("head_id", "id")));
        assert_eq!(lines.foreign_keys[0].on_delete.as_deref(), Some("set null"));
        assert_eq!(lines.indexes[0].columns, ["head_id", "n"]);
        assert_eq!(lines.columns[1].default.as_deref(), Some("1"));
        conn.execute(
            "drop table xenon_lines, xenon_heads; drop type xenon_mood",
            10,
        )
        .unwrap();
    }

    #[test]
    fn live_cancel() {
        let Some(db) = live() else { return };
        let cancel = db.cancel_handle();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            cancel.cancel();
        });
        let err = db
            .execute("select pg_sleep(10)", 10)
            .unwrap_err()
            .to_string();
        canceller.join().unwrap();
        assert!(err.contains("57014"), "{err}");
    }

    /// Expects the test server to use a self-signed certificate, as a local test server does.
    /// The fleet's throwaway certificates (dev/fleet/tls-setup/make.sh).
    fn fleet_tls(file: &str) -> String {
        format!("{}/../../dev/fleet/tls/{file}", env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn live_mutual_tls() {
        if std::env::var("XENON_TEST_PG_URL").is_err() {
            return;
        }
        let base = ConnectOptions {
            host: "127.0.0.1".into(),
            port: Some(55432),
            database: "shop".into(),
            ssl: SslMode::VerifyFull,
            ..Default::default()
        };
        let ca = || TlsFiles {
            ca: fleet_tls("ca.pem"),
            ..Default::default()
        };
        // Given the fleet's CA, verify-full checks the server and passes.
        PostgresConnection::connect_options(&ConnectOptions {
            user: "xenon".into(),
            password: "xenon".into(),
            tls: ca(),
            ..base.clone()
        })
        .expect("verify-full with the fleet's CA");
        // `xenon_cert` logs in with its client certificate, no password.
        let cert = ConnectOptions {
            user: "xenon_cert".into(),
            tls: TlsFiles {
                cert: fleet_tls("client.pem"),
                key: fleet_tls("client.key"),
                ..ca()
            },
            ..base.clone()
        };
        let conn = PostgresConnection::connect_options(&cert).expect("client certificate login");
        let who = rows(conn.execute("select current_user", 1).unwrap());
        assert_eq!(who.rows[0][0], Value::Text("xenon_cert".into()));
        // Without the certificate, the server turns it away.
        assert!(
            PostgresConnection::connect_options(&ConnectOptions { tls: ca(), ..cert }).is_err(),
            "logged in without a client certificate"
        );
    }

    #[test]
    fn live_verify_full_rejects_self_signed_certificates() {
        let Ok(url) = std::env::var("XENON_TEST_PG_URL") else {
            return;
        };
        let (url, _) = extract_sslmode(&url);
        let url = url
            .replace("sslmode=require", "")
            .trim_end_matches(['?', '&'])
            .to_string();
        let separator = if url.contains('?') { '&' } else { '?' };
        assert!(
            PostgresConnection::connect_url(&format!("{url}{separator}sslmode=require")).is_ok()
        );
        let err = PostgresConnection::connect_url(&format!("{url}{separator}sslmode=verify-full"))
            .err()
            .expect("self-signed certificate must be rejected");
        assert!(
            err.to_string().to_lowercase().contains("certificate"),
            "{err}"
        );
    }

    #[test]
    fn live_introspect_lists_public_first() {
        let Some(db) = live() else { return };
        db.execute("create table if not exists xenon_introspect (id serial primary key, note text not null)", 10)
            .unwrap();
        let schemas = db.introspect().unwrap();
        assert_eq!(schemas[0].name, "public");
        let table = schemas[0]
            .objects
            .iter()
            .find(|o| o.name == "xenon_introspect")
            .expect("table listed");
        assert!(table.columns[0].primary_key);
        assert!(table.columns[1].not_null);
        assert_eq!(table.columns[1].type_name, "text");
        db.execute("drop table xenon_introspect", 10).unwrap();
    }
}
