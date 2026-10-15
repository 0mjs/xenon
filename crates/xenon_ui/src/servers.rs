//! Per-engine helpers for server connections: parsing URLs, connecting, and
//! the short descriptions shown on cards and in lists.

use std::sync::Arc;

use xenon_driver::{ConnectOptions, Connection};
use xenon_store::ServerEngine;

use crate::widgets::Engine;

pub fn parse_url(engine: ServerEngine, url: &str) -> anyhow::Result<ConnectOptions> {
    match engine {
        ServerEngine::Postgres => xenon_postgres::parse_url(url),
        ServerEngine::Mysql => xenon_mysql::parse_url(url),
        ServerEngine::SqlServer => xenon_mssql::parse_url(url),
    }
}

/// Blocking: call off the main thread. Opens the SSH tunnel first when there is one.
pub fn connect(
    engine: ServerEngine,
    options: &ConnectOptions,
) -> anyhow::Result<Arc<dyn Connection>> {
    let Some(ssh) = &options.ssh else {
        return connect_direct(engine, options);
    };
    let host = or(&options.host, "localhost").to_string();
    let port = options.port.unwrap_or(engine.default_port());
    let tunnel = xenon_ssh::Tunnel::open(ssh, &host, port, &known_hosts())?;
    let local = ConnectOptions {
        host: host.clone(),
        port: Some(port),
        ssh: None,
        address: Some(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            tunnel.local_port(),
        ))),
        ..options.clone()
    };
    let inner = connect_direct(engine, &local).map_err(|err| {
        // The driver only sees a reset; ssh knows why (e.g. the database host
        // can't be reached from the SSH server).
        match tunnel.forward_failure() {
            Some(reason) => anyhow::anyhow!(
                "Connected to the SSH server {}, but it couldn't reach the database at {host}:{port}.\n\nssh: {reason}",
                tunnel.destination()
            ),
            None => err,
        }
    })?;
    let via = tunnel.destination().to_string();
    Ok(Arc::new(Tunneled {
        id: format!(
            "{}:{} via {via}",
            engine.as_str(),
            describe_options(engine, options)
        ),
        name: default_name(engine, options),
        via,
        inner,
        _tunnel: tunnel,
    }))
}

/// Xenon's own known_hosts, next to its other data. Hosts in ~/.ssh/known_hosts
/// are trusted too; new ones are recorded here rather than there.
fn known_hosts() -> std::path::PathBuf {
    xenon_store::Store::default_path()
        .ok()
        .and_then(|path| path.parent().map(|dir| dir.join("known_hosts")))
        .unwrap_or_else(|| std::env::temp_dir().join("xenon-known_hosts"))
}

/// A connection through an SSH tunnel: the database driver talks to a local
/// port, but its identity (for history) and name are the real server's.
struct Tunneled {
    id: String,
    name: String,
    via: String,
    inner: Arc<dyn Connection>,
    _tunnel: xenon_ssh::Tunnel,
}

impl Connection for Tunneled {
    fn name(&self) -> &str {
        &self.name
    }
    fn id(&self) -> String {
        self.id.clone()
    }
    fn server_version(&self) -> String {
        self.inner.server_version()
    }
    fn execute(&self, sql: &str, row_limit: usize) -> anyhow::Result<xenon_driver::QueryResult> {
        self.inner.execute(sql, row_limit)
    }
    fn introspect(&self) -> anyhow::Result<Vec<xenon_driver::Schema>> {
        self.inner.introspect()
    }
    fn cancel_handle(&self) -> Arc<dyn xenon_driver::CancelHandle> {
        self.inner.cancel_handle()
    }
    fn explain(&self, sql: &str) -> String {
        self.inner.explain(sql)
    }
    fn quote_ident(&self, ident: &str) -> String {
        self.inner.quote_ident(ident)
    }
    fn ident(&self, ident: &str) -> String {
        self.inner.ident(ident)
    }
    fn default_schema(&self) -> Option<String> {
        self.inner.default_schema()
    }
    fn uses_top(&self) -> bool {
        self.inner.uses_top()
    }
    fn literal(&self, value: &xenon_driver::Value) -> String {
        self.inner.literal(value)
    }
    fn begin(&self) -> &'static str {
        self.inner.begin()
    }
    fn tunnel(&self) -> Option<String> {
        Some(self.via.clone())
    }
}

fn connect_direct(
    engine: ServerEngine,
    options: &ConnectOptions,
) -> anyhow::Result<Arc<dyn Connection>> {
    Ok(match engine {
        ServerEngine::Postgres => Arc::new(xenon_postgres::PostgresConnection::connect_options(
            options,
        )?),
        ServerEngine::Mysql => Arc::new(xenon_mysql::MysqlConnection::connect_options(options)?),
        ServerEngine::SqlServer => {
            Arc::new(xenon_mssql::MssqlConnection::connect_options(options)?)
        }
    })
}

/// The logo for an open connection: MariaDB only shows itself once connected.
pub fn logo(engine: ServerEngine, conn: &dyn Connection) -> Engine {
    match engine {
        ServerEngine::Mysql if conn.server_version().starts_with("mariadb") => Engine::MariaDb,
        engine => Engine::from_server(engine),
    }
}

fn or<'a>(value: &'a str, default: &'a str) -> &'a str {
    if value.is_empty() { default } else { value }
}

/// `user@host:port/database`, with each engine's defaults filled in and no password.
/// The user a server assumes when none is given (Postgres: your account name).
pub fn default_user(engine: ServerEngine) -> String {
    match engine {
        ServerEngine::Postgres => xenon_postgres::default_user(),
        engine => engine.default_user().to_string(),
    }
}

pub fn describe(
    engine: ServerEngine,
    host: &str,
    port: Option<u16>,
    database: &str,
    user: &str,
) -> String {
    let host = or(host, "localhost");
    let fallback = default_user(engine);
    let user = or(user, &fallback);
    let port = port.unwrap_or(engine.default_port());
    match (engine, database) {
        (ServerEngine::Mysql | ServerEngine::SqlServer, "") => format!("{user}@{host}:{port}"),
        _ => format!("{user}@{host}:{port}/{}", or(database, "postgres")),
    }
}

pub fn describe_options(engine: ServerEngine, options: &ConnectOptions) -> String {
    describe(
        engine,
        &options.host,
        options.port,
        &options.database,
        &options.user,
    )
}

/// A name for a connection nobody named: `database@host`, or `user@host`.
pub fn default_name(engine: ServerEngine, options: &ConnectOptions) -> String {
    let host = or(&options.host, "localhost");
    match (engine, options.database.as_str()) {
        (ServerEngine::Mysql | ServerEngine::SqlServer, "") => {
            format!("{}@{host}", or(&options.user, engine.default_user()))
        }
        (_, database) => format!("{}@{host}", or(database, "postgres")),
    }
}

/// A saved tunnel as connection settings (the password comes from the Keychain).
pub fn ssh_options(tunnel: &xenon_store::SshTunnel) -> xenon_driver::SshOptions {
    xenon_driver::SshOptions {
        host: tunnel.host.clone(),
        port: tunnel.port,
        user: tunnel.user.clone(),
        key_file: tunnel.key_file.clone(),
        password: String::new(),
    }
}

/// Connection settings for a tunnel as saved (without the password).
pub fn ssh_tunnel(options: &xenon_driver::SshOptions) -> xenon_store::SshTunnel {
    xenon_store::SshTunnel {
        host: options.host.clone(),
        port: options.port,
        user: options.user.clone(),
        key_file: options.key_file.clone(),
    }
}

pub fn url_example(engine: ServerEngine) -> &'static str {
    match engine {
        ServerEngine::Postgres => "postgres://user:password@host:5432/database",
        ServerEngine::Mysql => "mysql://user:password@host:3306/database",
        ServerEngine::SqlServer => {
            "mssql://user:password@host:1433/database, or Server=…;Database=…"
        }
    }
}

/// Through the test fleet's bastion: `dev/fleet/up.sh bastion postgres mysql`,
/// then `cargo test -p xenon_ui servers -- --ignored`.
#[cfg(test)]
mod live {
    use super::*;
    use xenon_driver::{QueryOutcome, SshOptions, Value};

    fn through_bastion(host: &str, port: u16) -> ConnectOptions {
        // Keep the bastion's host key out of the real Xenon data folder.
        static DATA: std::sync::Once = std::sync::Once::new();
        DATA.call_once(|| {
            let dir = std::env::temp_dir().join(format!("xenon-servers-{}", std::process::id()));
            // SAFETY: runs once, before any test reads the environment.
            unsafe { std::env::set_var("XENON_DATA_DIR", dir) };
        });
        ConnectOptions {
            host: host.into(),
            port: Some(port),
            database: "shop".into(),
            user: "xenon".into(),
            password: "xenon".into(),
            ssh: Some(SshOptions {
                host: "127.0.0.1".into(),
                port: Some(52222),
                user: "xenon".into(),
                key_file: concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../dev/fleet/ssh/id_ed25519"
                )
                .into(),
                password: String::new(),
            }),
            ..Default::default()
        }
    }

    fn orders(conn: &dyn Connection) -> Value {
        match conn
            .execute("select count(*) from orders", 10)
            .unwrap()
            .outcome
        {
            QueryOutcome::Rows(rows) => rows.rows[0][0].clone(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    #[ignore]
    fn postgres_and_mysql_through_a_bastion() {
        // Hosts are the fleet's service names: only the bastion can reach them.
        let pg = connect(ServerEngine::Postgres, &through_bastion("postgres", 5432)).unwrap();
        assert!(matches!(orders(pg.as_ref()), Value::Integer(3000)));
        assert_eq!(pg.tunnel().as_deref(), Some("127.0.0.1:52222"));
        assert_eq!(
            pg.id(),
            "postgres:xenon@postgres:5432/shop via 127.0.0.1:52222"
        );
        assert!(!pg.is_local());

        let my = connect(ServerEngine::Mysql, &through_bastion("mysql", 3306)).unwrap();
        assert!(matches!(orders(my.as_ref()), Value::Integer(3000)));
    }

    /// A Homebrew or Postgres.app server on 5432 has a role named after you,
    /// not `postgres`; empty fields must work, as they do for psql.
    #[test]
    #[ignore]
    fn local_postgres_with_empty_fields() {
        if std::net::TcpStream::connect(("127.0.0.1", 5432)).is_err() {
            return;
        }
        let options = ConnectOptions {
            host: "localhost".into(),
            port: Some(5432),
            ..Default::default()
        };
        let conn = connect(ServerEngine::Postgres, &options).unwrap();
        assert!(conn.server_version().starts_with("postgres "));
    }

    #[test]
    #[ignore]
    fn an_unreachable_database_says_why() {
        let err = connect(
            ServerEngine::Postgres,
            &through_bastion("no-such-host", 5432),
        )
        .err()
        .unwrap();
        let message = format!("{err:#}");
        assert!(
            message.contains("couldn't reach the database at no-such-host:5432"),
            "{message}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptions_fill_in_defaults() {
        // Like psql, no user means your account name; no database means `postgres`.
        let me = xenon_postgres::default_user();
        assert_eq!(
            describe(ServerEngine::Postgres, "", None, "", ""),
            format!("{me}@localhost:5432/postgres")
        );
        assert_eq!(
            describe(ServerEngine::Mysql, "db", Some(3307), "", "app"),
            "app@db:3307"
        );
        assert_eq!(
            describe(ServerEngine::Mysql, "", None, "shop", ""),
            "root@localhost:3306/shop"
        );
        let options = ConnectOptions {
            host: "db".into(),
            ..Default::default()
        };
        assert_eq!(default_name(ServerEngine::Mysql, &options), "root@db");
        assert_eq!(
            default_name(ServerEngine::Postgres, &options),
            "postgres@db"
        );
    }
}
