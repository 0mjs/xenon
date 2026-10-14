//! Working out what a pasted connection string, env line or file path points at.

use std::path::PathBuf;

use xenon_store::ServerEngine;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Detected {
    /// A server URL (or a libpq `key=value` string for Postgres), ready to connect with.
    Server(ServerEngine, String),
    Sqlite(PathBuf),
    /// Recognised, but Xenon can't open it yet (e.g. "SQL Server").
    NotYet(&'static str),
    Unknown,
}

/// Recognise `text`: a URL (`postgres://…`, `mysql://…`), a `DATABASE_URL=…`
/// line (with or without `export` and quotes), a libpq `host=… dbname=…`
/// string, or a path to a database file (`~` expanded).
pub fn detect(text: &str) -> Detected {
    let text = text.trim();
    if is_libpq(&text.to_ascii_lowercase()) {
        return Detected::Server(ServerEngine::Postgres, text.to_string());
    }
    if is_ado(text) {
        return Detected::Server(ServerEngine::SqlServer, text.to_string());
    }
    let value = env_value(text);
    if is_ado(value) {
        return Detected::Server(ServerEngine::SqlServer, value.to_string());
    }
    let lower = value.to_ascii_lowercase();

    let scheme = lower.split("://").next().unwrap_or("");
    if lower.contains("://") {
        return match scheme {
            "postgres" | "postgresql" => {
                Detected::Server(ServerEngine::Postgres, value.to_string())
            }
            "sqlite" | "sqlite3" => {
                Detected::Sqlite(expand_home(value.split_once("://").map_or("", |(_, p)| p)))
            }
            "mysql" | "mariadb" => Detected::Server(ServerEngine::Mysql, value.to_string()),
            "mysqlx" => Detected::NotYet("MySQL X Protocol"),
            "sqlserver" | "mssql" | "jdbc:sqlserver" => {
                Detected::Server(ServerEngine::SqlServer, value.to_string())
            }
            "duckdb" => Detected::NotYet("DuckDB"),
            "mongodb" | "mongodb+srv" => Detected::NotYet("MongoDB"),
            "redis" | "rediss" => Detected::NotYet("Redis"),
            _ => Detected::Unknown,
        };
    }
    if lower.starts_with("sqlite:") {
        return Detected::Sqlite(expand_home(&value["sqlite:".len()..]));
    }

    let looks_like_path =
        value.starts_with('/') || value.starts_with("~/") || value.starts_with("./");
    let extension = lower.rsplit_once('.').map_or("", |(_, ext)| ext);
    match extension {
        "db" | "sqlite" | "sqlite3" | "db3" | "s3db" | "sl3" => {
            Detected::Sqlite(expand_home(value))
        }
        "duckdb" | "ddb" | "parquet" | "csv" | "tsv" => Detected::NotYet("DuckDB"),
        _ if looks_like_path => Detected::Sqlite(expand_home(value)),
        _ => Detected::Unknown,
    }
}

/// `DATABASE_URL="postgres://…"` or `export PG_URL=postgres://…` → the value.
/// Anything whose text before `=` isn't a plain identifier is left alone, so
/// URLs with `?sslmode=…` pass through.
fn env_value(text: &str) -> &str {
    let line = text.strip_prefix("export ").unwrap_or(text).trim();
    let value = match line.split_once('=') {
        Some((key, value))
            if !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') =>
        {
            value
        }
        _ => line,
    };
    value.trim().trim_matches(|c| c == '"' || c == '\'').trim()
}

/// An ADO.NET connection string: `Server=tcp:host,1433;Database=…;User Id=…`.
fn is_ado(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains(';')
        && !lower.contains("://")
        && (lower.starts_with("server=")
            || lower.starts_with("data source=")
            || lower.contains(";server=")
            || lower.contains(";data source="))
}

fn is_libpq(lower: &str) -> bool {
    let keys = [
        "host=",
        "dbname=",
        "user=",
        "port=",
        "hostaddr=",
        "service=",
    ];
    lower
        .split_whitespace()
        .filter(|part| keys.iter().any(|k| part.starts_with(k)))
        .count()
        >= 2
}

pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().map_or_else(|| PathBuf::from(path), |home| home.join(rest)),
        None => PathBuf::from(path),
    }
}

/// `/Users/me/dev/app.db` → `~/dev/app.db`, for display.
pub fn tilde(path: &str) -> String {
    match dirs::home_dir().and_then(|home| path.strip_prefix(home.to_str()?).map(str::to_string)) {
        Some(rest) => format!("~{rest}"),
        None => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pg(url: &str) -> Detected {
        Detected::Server(ServerEngine::Postgres, url.into())
    }

    #[test]
    fn urls() {
        assert_eq!(
            detect("postgres://u:p@db:5432/app"),
            pg("postgres://u:p@db:5432/app")
        );
        assert_eq!(
            detect("  postgresql://localhost/app?sslmode=require "),
            pg("postgresql://localhost/app?sslmode=require")
        );
        assert_eq!(
            detect("mysql://root@localhost/shop"),
            Detected::Server(ServerEngine::Mysql, "mysql://root@localhost/shop".into())
        );
        assert_eq!(
            detect("mariadb://x/y"),
            Detected::Server(ServerEngine::Mysql, "mariadb://x/y".into())
        );
        assert_eq!(
            detect("sqlserver://host;database=x"),
            Detected::Server(
                ServerEngine::SqlServer,
                "sqlserver://host;database=x".into()
            )
        );
        let ado = "Server=tcp:db,1433;Database=shop;User Id=sa;Password=pw";
        assert_eq!(
            detect(ado),
            Detected::Server(ServerEngine::SqlServer, ado.into())
        );
        assert_eq!(
            detect(&format!("ConnectionStrings__Default=\"{ado}\"")),
            Detected::Server(ServerEngine::SqlServer, ado.into())
        );
        assert_eq!(
            detect("mongodb+srv://cluster/db"),
            Detected::NotYet("MongoDB")
        );
        assert_eq!(detect("https://example.com"), Detected::Unknown);
    }

    #[test]
    fn env_lines() {
        let want = pg("postgres://u:p@db/app");
        assert_eq!(detect("DATABASE_URL=postgres://u:p@db/app"), want);
        assert_eq!(detect("DATABASE_URL=\"postgres://u:p@db/app\""), want);
        assert_eq!(detect("export PG_URL='postgres://u:p@db/app'"), want);
        assert_eq!(
            detect("DB_URL=mysql://root@db/shop"),
            Detected::Server(ServerEngine::Mysql, "mysql://root@db/shop".into())
        );
    }

    #[test]
    fn urls_with_equals_in_the_query_are_not_env_lines() {
        assert_eq!(
            detect("postgres://db/app?sslmode=require"),
            pg("postgres://db/app?sslmode=require")
        );
    }

    #[test]
    fn libpq_strings() {
        assert_eq!(
            detect("host=localhost dbname=app user=me"),
            pg("host=localhost dbname=app user=me")
        );
        assert_eq!(detect("host=localhost"), Detected::Unknown);
    }

    #[test]
    fn files() {
        assert_eq!(
            detect("/data/app.db"),
            Detected::Sqlite("/data/app.db".into())
        );
        assert_eq!(
            detect("sqlite:///data/app.sqlite"),
            Detected::Sqlite("/data/app.sqlite".into())
        );
        assert_eq!(
            detect("./local.sqlite3"),
            Detected::Sqlite("./local.sqlite3".into())
        );
        assert_eq!(detect("events.parquet"), Detected::NotYet("DuckDB"));
        assert_eq!(detect("warehouse.duckdb"), Detected::NotYet("DuckDB"));
        let home = dirs::home_dir().unwrap();
        assert_eq!(
            detect("~/dev/app.db"),
            Detected::Sqlite(home.join("dev/app.db"))
        );
        assert_eq!(
            tilde(&home.join("dev/app.db").display().to_string()),
            "~/dev/app.db"
        );
    }

    #[test]
    fn nonsense() {
        assert_eq!(detect(""), Detected::Unknown);
        assert_eq!(detect("hello world"), Detected::Unknown);
    }
}
