//! Xenon's local state, kept in a SQLite file in the platform data directory
//! (`~/Library/Application Support/Xenon/xenon.db` on macOS). Passwords never
//! go in that file; see [`secrets`].

pub mod secrets;

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow};
use rusqlite::{OptionalExtension as _, params};

/// Entries kept per connection; older ones are pruned.
const HISTORY_LIMIT: usize = 5_000;

#[derive(Clone, Debug, PartialEq)]
pub struct HistoryEntry {
    pub id: i64,
    pub connection: String,
    pub sql: String,
    /// Milliseconds since the Unix epoch.
    pub ran_at: i64,
    pub duration_ms: i64,
    /// Rows returned or affected; `None` when the query failed.
    pub rows: Option<i64>,
    pub error: Option<String>,
}

pub struct NewHistoryEntry<'a> {
    pub connection: &'a str,
    pub sql: &'a str,
    pub duration: Duration,
    pub rows: Option<u64>,
    pub error: Option<&'a str>,
}

/// Where a connection's data lives and how risky it is to change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Environment {
    #[default]
    None,
    Development,
    Production,
}

impl Environment {
    pub const ALL: [Environment; 3] = [
        Environment::None,
        Environment::Development,
        Environment::Production,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Environment::None => "none",
            Environment::Development => "dev",
            Environment::Production => "prod",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "dev" => Environment::Development,
            "prod" => Environment::Production,
            _ => Environment::None,
        }
    }
}

/// Database servers Xenon can connect to over the network.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ServerEngine {
    Postgres,
    /// MySQL and MariaDB (and wire-compatible servers such as TiDB).
    Mysql,
    /// SQL Server and Azure SQL.
    SqlServer,
}

impl ServerEngine {
    pub const ALL: [ServerEngine; 3] = [
        ServerEngine::Postgres,
        ServerEngine::Mysql,
        ServerEngine::SqlServer,
    ];

    /// The value stored in the `kind` column.
    pub fn as_str(self) -> &'static str {
        match self {
            ServerEngine::Postgres => "postgres",
            ServerEngine::Mysql => "mysql",
            ServerEngine::SqlServer => "mssql",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        ServerEngine::ALL
            .into_iter()
            .find(|engine| engine.as_str() == value)
    }

    pub fn label(self) -> &'static str {
        match self {
            ServerEngine::Postgres => "Postgres",
            ServerEngine::Mysql => "MySQL",
            ServerEngine::SqlServer => "SQL Server",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            ServerEngine::Postgres => 5432,
            ServerEngine::Mysql => 3306,
            ServerEngine::SqlServer => 1433,
        }
    }

    pub fn default_user(self) -> &'static str {
        match self {
            ServerEngine::Postgres => "postgres",
            ServerEngine::Mysql => "root",
            ServerEngine::SqlServer => "sa",
        }
    }
}

/// An SSH server a saved connection tunnels through. The password or key
/// passphrase, if any, is in the Keychain under [`SavedConnection::ssh_secret_key`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SshTunnel {
    pub host: String,
    pub port: Option<u16>,
    pub user: String,
    pub key_file: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Sqlite {
        path: String,
    },
    Server {
        engine: ServerEngine,
        host: String,
        port: Option<u16>,
        database: String,
        user: String,
        ssl: String,
        ssh: Option<SshTunnel>,
    },
}

/// A connection the user saved. The password, if any, is in the Keychain
/// under [`SavedConnection::secret_key`].
#[derive(Clone, Debug, PartialEq)]
pub struct SavedConnection {
    pub id: i64,
    pub name: String,
    pub target: Target,
    pub environment: Environment,
    pub read_only: bool,
    /// A colour to tell it apart by (a name like `blue`; empty for none).
    pub color: String,
}

impl SavedConnection {
    pub fn secret_key(id: i64) -> String {
        format!("connection-{id}")
    }

    pub fn ssh_secret_key(id: i64) -> String {
        format!("connection-{id}-ssh")
    }
}

/// A query the user named and kept.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedQuery {
    pub id: i64,
    pub name: String,
    pub sql: String,
    /// The saved connection it was written for, if any.
    pub connection: Option<i64>,
    /// Milliseconds since the Unix epoch.
    pub updated_at: i64,
}

/// A tab that was open when Xenon last quit, to reopen on launch.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OpenTab {
    /// The saved connection the tab ran on.
    pub connection: i64,
    pub title: String,
    pub sql: String,
    /// A table tab: the table and its WHERE filter.
    pub table: Option<(String, String)>,
    pub filter: String,
    pub active: bool,
    pub saved_query: Option<i64>,
}

pub struct Store {
    conn: Mutex<rusqlite::Connection>,
}

impl Store {
    /// `~/Library/Application Support/Xenon/xenon.db`, or `$XENON_DATA_DIR/xenon.db`
    /// when set (tests and the snapshot tool use a throwaway directory).
    pub fn default_path() -> Result<PathBuf> {
        if let Some(dir) = std::env::var_os("XENON_DATA_DIR") {
            return Ok(PathBuf::from(dir).join("xenon.db"));
        }
        let dir = dirs::data_dir()
            .ok_or_else(|| anyhow!("no data directory on this platform"))?
            .join("Xenon");
        Ok(dir.join("xenon.db"))
    }

    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("could not create {}", dir.display()))?;
        }
        let conn = rusqlite::Connection::open(path)
            .with_context(|| format!("could not open {}", path.display()))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(rusqlite::Connection::open_in_memory()?)
    }

    fn init(conn: rusqlite::Connection) -> Result<Self> {
        conn.execute_batch(
            "pragma journal_mode = wal;
             create table if not exists history (
                 id integer primary key,
                 connection text not null,
                 sql text not null,
                 ran_at integer not null,
                 duration_ms integer not null,
                 rows integer,
                 error text
             );
             create index if not exists history_by_connection on history (connection, ran_at desc);
             create table if not exists settings (key text primary key, value text not null);
             create table if not exists connections (
                 id integer primary key,
                 name text not null,
                 kind text not null,
                 host text not null default '',
                 port integer,
                 database text not null default '',
                 user text not null default '',
                 ssl text not null default 'prefer',
                 path text not null default '',
                 environment text not null default 'none',
                 read_only integer not null default 0,
                 last_used integer not null default 0
             );
             create table if not exists saved_queries (
                 id integer primary key,
                 name text not null,
                 sql text not null,
                 connection integer,
                 updated_at integer not null
             );
             create table if not exists open_tabs (
                 position integer primary key,
                 connection integer not null,
                 title text not null,
                 sql text not null,
                 schema_name text,
                 table_name text,
                 filter text not null default '',
                 active integer not null default 0,
                 saved_query integer
             );",
        )?;
        // Columns added after the table first shipped.
        let columns: Vec<String> = conn
            .prepare("select name from pragma_table_info('connections')")?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for (column, definition) in [
            ("ssh_host", "text not null default ''"),
            ("ssh_port", "integer"),
            ("ssh_user", "text not null default ''"),
            ("ssh_key", "text not null default ''"),
            ("color", "text not null default ''"),
        ] {
            if !columns.iter().any(|c| c == column) {
                conn.execute_batch(&format!(
                    "alter table connections add column {column} {definition}"
                ))?;
            }
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, rusqlite::Connection> {
        // A panic mid-write leaves SQLite consistent, so a poisoned lock is safe to reuse.
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn record(&self, entry: NewHistoryEntry<'_>) -> Result<HistoryEntry> {
        let ran_at = now_ms();
        let duration_ms = entry.duration.as_millis() as i64;
        let rows = entry.rows.map(|n| n as i64);

        let conn = self.lock();
        conn.execute(
            "insert into history (connection, sql, ran_at, duration_ms, rows, error)
             values (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                entry.connection,
                entry.sql,
                ran_at,
                duration_ms,
                rows,
                entry.error
            ],
        )?;
        let id = conn.last_insert_rowid();
        conn.execute(
            "delete from history where connection = ?1 and id not in (
                 select id from history where connection = ?1 order by ran_at desc, id desc limit ?2
             )",
            params![entry.connection, HISTORY_LIMIT as i64],
        )?;

        Ok(HistoryEntry {
            id,
            connection: entry.connection.to_string(),
            sql: entry.sql.to_string(),
            ran_at,
            duration_ms,
            rows,
            error: entry.error.map(str::to_string),
        })
    }

    /// Saved connections, most recently used first.
    pub fn connections(&self) -> Result<Vec<SavedConnection>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "select id, name, kind, host, port, database, user, ssl, path, environment, read_only,
                    ssh_host, ssh_port, ssh_user, ssh_key, color
             from connections order by last_used desc, id desc",
        )?;
        let rows = stmt
            .query_map([], |row| {
                let kind: String = row.get(2)?;
                let target = if kind == "sqlite" {
                    Target::Sqlite { path: row.get(8)? }
                } else {
                    Target::Server {
                        // Unknown kinds come from a newer Xenon; Postgres was the first.
                        engine: ServerEngine::parse(&kind).unwrap_or(ServerEngine::Postgres),
                        host: row.get(3)?,
                        port: row.get::<_, Option<i64>>(4)?.map(|p| p as u16),
                        database: row.get(5)?,
                        user: row.get(6)?,
                        ssl: row.get(7)?,
                        ssh: {
                            let host: String = row.get(11)?;
                            (!host.is_empty())
                                .then(|| -> rusqlite::Result<SshTunnel> {
                                    Ok(SshTunnel {
                                        host,
                                        port: row.get::<_, Option<i64>>(12)?.map(|p| p as u16),
                                        user: row.get(13)?,
                                        key_file: row.get(14)?,
                                    })
                                })
                                .transpose()?
                        },
                    }
                };
                Ok(SavedConnection {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    target,
                    environment: Environment::parse(&row.get::<_, String>(9)?),
                    read_only: row.get(10)?,
                    color: row.get(15)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Insert (`id` 0) or update a saved connection; returns its id. Saving a
    /// SQLite file that is already saved updates that entry instead of adding one.
    pub fn save_connection(&self, saved: &SavedConnection) -> Result<i64> {
        let conn = self.lock();
        let no_tunnel = SshTunnel::default();
        let (kind, host, port, database, user, ssl, path, ssh) = match &saved.target {
            Target::Sqlite { path } => (
                "sqlite",
                "",
                None,
                "",
                "",
                "prefer",
                path.as_str(),
                &no_tunnel,
            ),
            Target::Server {
                engine,
                host,
                port,
                database,
                user,
                ssl,
                ssh,
            } => (
                engine.as_str(),
                host.as_str(),
                port.map(i64::from),
                database.as_str(),
                user.as_str(),
                ssl.as_str(),
                "",
                ssh.as_ref().unwrap_or(&no_tunnel),
            ),
        };
        let ssh_port = ssh.port.map(i64::from);
        let mut id = saved.id;
        if id == 0 && kind == "sqlite" {
            id = conn
                .query_row(
                    "select id from connections where kind = 'sqlite' and path = ?1",
                    [path],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or(0);
        }
        let environment = saved.environment.as_str();
        if id == 0 {
            conn.execute(
                "insert into connections
                     (name, kind, host, port, database, user, ssl, path, environment, read_only, last_used,
                      ssh_host, ssh_port, ssh_user, ssh_key, color)
                 values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![saved.name, kind, host, port, database, user, ssl, path, environment, saved.read_only, next_stamp(&conn)?,
                        ssh.host, ssh_port, ssh.user, ssh.key_file, saved.color],
            )?;
            Ok(conn.last_insert_rowid())
        } else {
            conn.execute(
                "update connections set name = ?1, kind = ?2, host = ?3, port = ?4, database = ?5,
                     user = ?6, ssl = ?7, path = ?8, environment = ?9, read_only = ?10, last_used = ?11,
                     ssh_host = ?12, ssh_port = ?13, ssh_user = ?14, ssh_key = ?15, color = ?16
                 where id = ?17",
                params![saved.name, kind, host, port, database, user, ssl, path, environment, saved.read_only, next_stamp(&conn)?,
                        ssh.host, ssh_port, ssh.user, ssh.key_file, saved.color, id],
            )?;
            Ok(id)
        }
    }

    pub fn touch_connection(&self, id: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "update connections set last_used = ?1 where id = ?2",
            params![next_stamp(&conn)?, id],
        )?;
        Ok(())
    }

    pub fn delete_connection(&self, id: i64) -> Result<()> {
        self.lock()
            .execute("delete from connections where id = ?1", [id])?;
        Ok(())
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .lock()
            .query_row("select value from settings where key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.lock().execute(
            "insert into settings (key, value) values (?1, ?2)
             on conflict (key) do update set value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    /// Forget every query run, on every connection.
    /// Newest first. `filter` matches the name or the SQL, case-insensitively.
    pub fn saved_queries(&self, filter: &str) -> Result<Vec<SavedQuery>> {
        let pattern = like_pattern(filter);
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "select id, name, sql, connection, updated_at from saved_queries
             where name like ?1 escape '\\' or sql like ?1 escape '\\'
             order by updated_at desc, id desc",
        )?;
        let queries = stmt
            .query_map(params![pattern], |row| {
                Ok(SavedQuery {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    sql: row.get(2)?,
                    connection: row.get(3)?,
                    updated_at: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(queries)
    }

    /// Insert (`id` 0) or update a saved query; returns its id.
    pub fn save_query(&self, query: &SavedQuery) -> Result<i64> {
        let conn = self.lock();
        let now = now_ms();
        if query.id == 0 {
            conn.execute(
                "insert into saved_queries (name, sql, connection, updated_at) values (?1, ?2, ?3, ?4)",
                params![query.name, query.sql, query.connection, now],
            )?;
            Ok(conn.last_insert_rowid())
        } else {
            conn.execute(
                "update saved_queries set name = ?2, sql = ?3, connection = ?4, updated_at = ?5 where id = ?1",
                params![query.id, query.name, query.sql, query.connection, now],
            )?;
            Ok(query.id)
        }
    }

    pub fn delete_query(&self, id: i64) -> Result<()> {
        self.lock()
            .execute("delete from saved_queries where id = ?1", params![id])?;
        Ok(())
    }

    /// The tabs to reopen, in order.
    pub fn open_tabs(&self) -> Result<Vec<OpenTab>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "select connection, title, sql, schema_name, table_name, filter, active, saved_query
             from open_tabs order by position",
        )?;
        let tabs = stmt
            .query_map([], |row| {
                let schema: Option<String> = row.get(3)?;
                let table: Option<String> = row.get(4)?;
                Ok(OpenTab {
                    connection: row.get(0)?,
                    title: row.get(1)?,
                    sql: row.get(2)?,
                    table: schema.zip(table),
                    filter: row.get(5)?,
                    active: row.get(6)?,
                    saved_query: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(tabs)
    }

    /// Replace the remembered tabs.
    pub fn set_open_tabs(&self, tabs: &[OpenTab]) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute("delete from open_tabs", [])?;
        for (position, tab) in tabs.iter().enumerate() {
            let (schema, table) = match &tab.table {
                Some((schema, table)) => (Some(schema), Some(table)),
                None => (None, None),
            };
            tx.execute(
                "insert into open_tabs (position, connection, title, sql, schema_name, table_name,
                                        filter, active, saved_query)
                 values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    position as i64,
                    tab.connection,
                    tab.title,
                    tab.sql,
                    schema,
                    table,
                    tab.filter,
                    tab.active,
                    tab.saved_query
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn clear_history(&self) -> Result<()> {
        self.lock().execute("delete from history", [])?;
        Ok(())
    }

    /// Newest first. `filter` matches anywhere in the SQL, case-insensitively.
    pub fn history(
        &self,
        connection: &str,
        filter: &str,
        limit: usize,
    ) -> Result<Vec<HistoryEntry>> {
        let pattern = like_pattern(filter);
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "select id, connection, sql, ran_at, duration_ms, rows, error from history
             where connection = ?1 and sql like ?2 escape '\\'
             order by ran_at desc, id desc limit ?3",
        )?;
        let entries = stmt
            .query_map(params![connection, pattern, limit as i64], |row| {
                Ok(HistoryEntry {
                    id: row.get(0)?,
                    connection: row.get(1)?,
                    sql: row.get(2)?,
                    ran_at: row.get(3)?,
                    duration_ms: row.get(4)?,
                    rows: row.get(5)?,
                    error: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(entries)
    }
}

/// A "last used" stamp later than any existing one, so two connections used in
/// the same millisecond still have a definite order.
fn next_stamp(conn: &rusqlite::Connection) -> Result<i64> {
    let latest: i64 = conn.query_row(
        "select coalesce(max(last_used), 0) from connections",
        [],
        |row| row.get(0),
    )?;
    Ok(now_ms().max(latest + 1))
}

/// A LIKE pattern matching `text` anywhere, with `\` escaping its wildcards.
fn like_pattern(text: &str) -> String {
    format!(
        "%{}%",
        text.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    )
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_have_nowhere_to_go_in_the_data_file() {
        // Secrets go to the Keychain (`secrets`); no table here can hold one.
        let store = Store::open_in_memory().unwrap();
        let conn = store.lock();
        let columns: Vec<String> = conn
            .prepare(
                "select m.name || '.' || c.name from sqlite_schema m \
                 join pragma_table_info(m.name) c where m.type = 'table'",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(!columns.is_empty());
        for column in columns {
            let lower = column.to_lowercase();
            assert!(
                !["pass", "secret", "token", "credential"]
                    .iter()
                    .any(|word| lower.contains(word)),
                "{column} could hold a secret"
            );
        }
    }

    #[test]
    fn saved_queries_insert_update_filter_delete() {
        let store = Store::open_in_memory().unwrap();
        let mut query = SavedQuery {
            id: 0,
            name: "Paid orders".into(),
            sql: "select * from orders where status = 'paid'".into(),
            connection: Some(3),
            updated_at: 0,
        };
        query.id = store.save_query(&query).unwrap();
        store
            .save_query(&SavedQuery {
                id: 0,
                name: "100% done".into(),
                sql: "select 1".into(),
                connection: None,
                updated_at: 0,
            })
            .unwrap();
        assert_eq!(store.saved_queries("").unwrap().len(), 2);
        assert_eq!(
            store.saved_queries("STATUS").unwrap()[0].name,
            "Paid orders"
        );
        assert_eq!(store.saved_queries("100%").unwrap().len(), 1);
        query.name = "Paid".into();
        assert_eq!(store.save_query(&query).unwrap(), query.id);
        assert_eq!(store.saved_queries("paid").unwrap()[0].name, "Paid");
        store.delete_query(query.id).unwrap();
        assert_eq!(store.saved_queries("").unwrap().len(), 1);
    }

    #[test]
    fn open_tabs_round_trip() {
        let store = Store::open_in_memory().unwrap();
        let tabs = vec![
            OpenTab {
                connection: 1,
                title: "Query 1".into(),
                sql: "select 1".into(),
                ..Default::default()
            },
            OpenTab {
                connection: 2,
                title: "orders".into(),
                sql: "select * from orders".into(),
                table: Some(("public".into(), "orders".into())),
                filter: "id = 3".into(),
                active: true,
                saved_query: None,
            },
        ];
        store.set_open_tabs(&tabs).unwrap();
        assert_eq!(store.open_tabs().unwrap(), tabs);
        store.set_open_tabs(&tabs[..1]).unwrap();
        assert_eq!(store.open_tabs().unwrap().len(), 1);
    }

    fn entry<'a>(connection: &'a str, sql: &'a str) -> NewHistoryEntry<'a> {
        NewHistoryEntry {
            connection,
            sql,
            duration: Duration::from_millis(12),
            rows: Some(3),
            error: None,
        }
    }

    #[test]
    fn records_and_lists_newest_first_per_connection() {
        let store = Store::open_in_memory().unwrap();
        store.record(entry("a", "select 1")).unwrap();
        store.record(entry("a", "select 2")).unwrap();
        store.record(entry("b", "select 3")).unwrap();

        let history = store.history("a", "", 10).unwrap();
        let sql: Vec<_> = history.iter().map(|e| e.sql.as_str()).collect();
        assert_eq!(sql, ["select 2", "select 1"]);
        assert_eq!(history[0].duration_ms, 12);
        assert_eq!(history[0].rows, Some(3));
        assert_eq!(store.history("b", "", 10).unwrap().len(), 1);
    }

    #[test]
    fn filter_is_a_literal_case_insensitive_substring() {
        let store = Store::open_in_memory().unwrap();
        store.record(entry("a", "SELECT * FROM users")).unwrap();
        store.record(entry("a", "select 100% from x_y")).unwrap();
        store.record(entry("a", "select 1")).unwrap();

        let find = |f| store.history("a", f, 10).unwrap().len();
        assert_eq!(find("users"), 1);
        assert_eq!(find("select"), 3);
        assert_eq!(find("%"), 1);
        assert_eq!(find("x_y"), 1);
        assert_eq!(find("xy"), 0);
    }

    #[test]
    fn records_failures() {
        let store = Store::open_in_memory().unwrap();
        store
            .record(NewHistoryEntry {
                rows: None,
                error: Some("ERROR 42703: column does not exist"),
                ..entry("a", "select emial")
            })
            .unwrap();
        let history = store.history("a", "", 10).unwrap();
        assert_eq!(history[0].rows, None);
        assert!(history[0].error.as_deref().unwrap().contains("42703"));
    }

    #[test]
    fn a_connection_keeps_its_colour() {
        let store = Store::open_in_memory().unwrap();
        let id = store
            .save_connection(&SavedConnection {
                id: 0,
                name: "Clients/Acme".into(),
                target: Target::Sqlite {
                    path: "/tmp/acme.db".into(),
                },
                environment: Environment::None,
                read_only: false,
                color: "blue".into(),
            })
            .unwrap();
        let saved = store.connections().unwrap();
        assert_eq!((saved[0].id, saved[0].color.as_str()), (id, "blue"));
    }

    #[test]
    fn saves_updates_and_deletes_connections() {
        let store = Store::open_in_memory().unwrap();
        let pg = SavedConnection {
            id: 0,
            name: "acme prod".into(),
            target: Target::Server {
                ssh: None,
                engine: ServerEngine::Postgres,
                host: "db.acme.dev".into(),
                port: Some(5432),
                database: "acme".into(),
                user: "app".into(),
                ssl: "require".into(),
            },
            environment: Environment::Production,
            read_only: true,
            color: String::new(),
        };
        let id = store.save_connection(&pg).unwrap();
        let file = SavedConnection {
            id: 0,
            name: "notes.db".into(),
            target: Target::Sqlite {
                path: "/tmp/notes.db".into(),
            },
            environment: Environment::None,
            read_only: false,
            color: String::new(),
        };
        let file_id = store.save_connection(&file).unwrap();
        // Re-saving the same file doesn't duplicate it.
        assert_eq!(store.save_connection(&file).unwrap(), file_id);

        let saved = store.connections().unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[0].id, file_id, "most recently used first");
        assert_eq!(saved[1], SavedConnection { id, ..pg.clone() });

        store
            .save_connection(&SavedConnection {
                id,
                name: "acme".into(),
                ..pg.clone()
            })
            .unwrap();
        store.touch_connection(id).unwrap();
        let saved = store.connections().unwrap();
        assert_eq!((saved[0].id, saved[0].name.as_str()), (id, "acme"));

        store.delete_connection(id).unwrap();
        assert_eq!(store.connections().unwrap().len(), 1);
    }

    #[test]
    fn settings_round_trip() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.setting("theme").unwrap(), None);
        store.set_setting("theme", "dark").unwrap();
        store.set_setting("theme", "light").unwrap();
        assert_eq!(store.setting("theme").unwrap().as_deref(), Some("light"));
    }

    #[test]
    fn persists_across_reopen() {
        let dir = std::env::temp_dir().join(format!("xenon-store-test-{}", std::process::id()));
        let path = dir.join("nested").join("xenon.db");
        Store::open(&path)
            .unwrap()
            .record(entry("a", "select 1"))
            .unwrap();
        assert_eq!(
            Store::open(&path)
                .unwrap()
                .history("a", "", 10)
                .unwrap()
                .len(),
            1
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ssh_tunnels_round_trip() {
        let store = Store::open_in_memory().unwrap();
        let tunnel = SshTunnel {
            host: "bastion.example.com".into(),
            port: Some(2222),
            user: "ops".into(),
            key_file: "~/.ssh/id_ed25519".into(),
        };
        let id = store
            .save_connection(&SavedConnection {
                id: 0,
                name: "Through the bastion".into(),
                target: Target::Server {
                    engine: ServerEngine::Mysql,
                    host: "10.0.0.5".into(),
                    port: None,
                    database: "shop".into(),
                    user: "app".into(),
                    ssl: "require".into(),
                    ssh: Some(tunnel.clone()),
                },
                environment: Environment::Production,
                read_only: true,
                color: String::new(),
            })
            .unwrap();
        let saved = &store.connections().unwrap()[0];
        assert_eq!(saved.id, id);
        assert!(
            matches!(&saved.target, Target::Server { ssh: Some(t), engine: ServerEngine::Mysql, .. } if *t == tunnel)
        );
    }

    #[test]
    fn older_databases_gain_the_ssh_columns() {
        let dir = std::env::temp_dir().join(format!("xenon-store-migrate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.db");
        let _ = std::fs::remove_file(&path);
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(
                "create table connections (id integer primary key, name text not null, kind text not null,
                   host text not null default '', port integer, database text not null default '',
                   user text not null default '', ssl text not null default 'prefer', path text not null default '',
                   environment text not null default 'none', read_only integer not null default 0,
                   last_used integer not null default 0);
                 insert into connections (name, kind, host) values ('old', 'postgres', 'db');",
            )
            .unwrap();
        let store = Store::open(&path).unwrap();
        let saved = store.connections().unwrap();
        assert!(matches!(&saved[0].target, Target::Server { ssh: None, .. }));
    }
}
