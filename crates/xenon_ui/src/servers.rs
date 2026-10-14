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
    }
}

/// Blocking: call off the main thread.
pub fn connect(
    engine: ServerEngine,
    options: &ConnectOptions,
) -> anyhow::Result<Arc<dyn Connection>> {
    Ok(match engine {
        ServerEngine::Postgres => Arc::new(xenon_postgres::PostgresConnection::connect_options(
            options,
        )?),
        ServerEngine::Mysql => Arc::new(xenon_mysql::MysqlConnection::connect_options(options)?),
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
pub fn describe(
    engine: ServerEngine,
    host: &str,
    port: Option<u16>,
    database: &str,
    user: &str,
) -> String {
    let host = or(host, "localhost");
    let user = or(user, engine.default_user());
    let port = port.unwrap_or(engine.default_port());
    match (engine, database) {
        (ServerEngine::Mysql, "") => format!("{user}@{host}:{port}"),
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
        (ServerEngine::Mysql, "") => format!("{}@{host}", or(&options.user, "root")),
        (_, database) => format!("{}@{host}", or(database, "postgres")),
    }
}

pub fn url_example(engine: ServerEngine) -> &'static str {
    match engine {
        ServerEngine::Postgres => "postgres://user:password@host:5432/database",
        ServerEngine::Mysql => "mysql://user:password@host:3306/database",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptions_fill_in_defaults() {
        assert_eq!(
            describe(ServerEngine::Postgres, "", None, "", ""),
            "postgres@localhost:5432/postgres"
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
