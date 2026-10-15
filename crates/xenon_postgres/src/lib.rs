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
    CancelHandle, Column, Connection, ObjectColumn, ObjectKind, QueryError, QueryOutcome,
    QueryResult, ResultSet, Schema, SchemaObject, Value,
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

/// Split a `postgres://` URL or libpq `key=value` string into fields, so it
/// can be saved (with the password kept apart, in the Keychain).
pub fn parse_url(url: &str) -> Result<ConnectOptions> {
    let (url, ssl) = extract_sslmode(url);
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
}

struct PgCancel {
    token: CancelToken,
    tls: MakeTlsConnector,
}

impl CancelHandle for PgCancel {
    fn cancel(&self) {
        let token = self.token.clone();
        let tls = self.tls.clone();
        runtime().spawn(async move {
            // The server doesn't report whether a cancel landed; nothing to do on failure.
            let _ = token.cancel_query(tls).await;
        });
    }
}

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
        let (url, ssl) = extract_sslmode(url);
        let mut config: Config = url.parse().context("invalid connection string")?;
        if let Some(ssl) = ssl {
            config.ssl_mode(wire_mode(ssl));
        }
        let ssl = ssl.unwrap_or(SslMode::Prefer);
        Self::connect(config, ssl)
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
            .ssl_mode(wire_mode(options.ssl));
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
        if options.read_only {
            config.options("-c default_transaction_read_only=on");
        }
        if let Some(address) = options.address {
            // Connect to the tunnel; `host` still names the server for TLS.
            config.hostaddr(address.ip());
        }
        Self::connect(config, options.ssl)
    }

    fn connect(mut config: Config, ssl: SslMode) -> Result<Self> {
        if config.get_connect_timeout().is_none() {
            config.connect_timeout(CONNECT_TIMEOUT);
        }
        let tls = tls_connector(ssl)?;
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
                }),
                client,
            })
        })
    }

    async fn run(&self, sql: &str, row_limit: usize) -> Result<QueryResult> {
        let started = Instant::now();
        // Prepare only to learn column types; multi-statement scripts can't be prepared.
        let types: Option<Vec<Type>> = self
            .client
            .prepare(sql)
            .await
            .ok()
            .map(|stmt| stmt.columns().iter().map(|c| c.type_().clone()).collect());

        let stream = self
            .client
            .simple_query_raw(sql)
            .await
            .map_err(describe_error)?;
        pin_mut!(stream);

        let mut statements = 0;
        let mut outcome = None;
        let mut current: Option<ResultSet> = None;
        let mut cancelled_for_limit = false;

        while let Some(message) = stream.next().await {
            let message = match message {
                Ok(message) => message,
                Err(err)
                    if cancelled_for_limit && err.code() == Some(&SqlState::QUERY_CANCELED) =>
                {
                    break;
                }
                Err(err) => return Err(describe_error(err)),
            };
            match message {
                SimpleQueryMessage::RowDescription(columns) => {
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
                        if !result.truncated {
                            // Stop the server instead of streaming rows we'd discard.
                            result.truncated = true;
                            cancelled_for_limit = true;
                            self.cancel.cancel();
                        }
                        continue;
                    }
                    result.rows.push(
                        (0..row.len())
                            .map(|ix| row.get(ix).map_or(Value::Null, |v| Value::Text(v.into())))
                            .collect(),
                    );
                }
                SimpleQueryMessage::CommandComplete(count) => {
                    statements += 1;
                    outcome = Some(match current.take() {
                        Some(rows) => QueryOutcome::Rows(rows),
                        None => QueryOutcome::Affected(count),
                    });
                }
                _ => {}
            }
        }
        if let Some(rows) = current.take() {
            // Cut short by the row limit: no CommandComplete arrives.
            statements += 1;
            outcome = Some(QueryOutcome::Rows(rows));
        }

        let mut outcome = outcome.ok_or_else(|| anyhow!("nothing to run"))?;
        if let (QueryOutcome::Rows(rows), Some(types), 1) = (&mut outcome, &types, statements)
            && types.len() == rows.columns.len()
        {
            apply_types(rows, types);
        }

        Ok(QueryResult {
            outcome,
            statements,
            elapsed: started.elapsed(),
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

    fn execute(&self, sql: &str, row_limit: usize) -> Result<QueryResult> {
        runtime().block_on(self.run(sql, row_limit))
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

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        self.cancel.clone()
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

fn tls_connector(ssl: SslMode) -> Result<MakeTlsConnector> {
    let mut builder = native_tls::TlsConnector::builder();
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
        Value::Text(text) => format!("'{}'", text.replace('\'', "''")),
        Value::Blob(bytes) => format!("'\\x{}'::bytea", xenon_driver::hex(bytes)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
