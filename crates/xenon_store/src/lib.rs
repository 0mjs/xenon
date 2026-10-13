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

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Sqlite {
        path: String,
    },
    Postgres {
        host: String,
        port: Option<u16>,
        database: String,
        user: String,
        ssl: String,
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
}

impl SavedConnection {
    pub fn secret_key(id: i64) -> String {
        format!("connection-{id}")
    }
}

pub struct Store {
    conn: Mutex<rusqlite::Connection>,
}

impl Store {
    pub fn default_path() -> Result<PathBuf> {
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
             );",
        )?;
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
            "select id, name, kind, host, port, database, user, ssl, path, environment, read_only
             from connections order by last_used desc, id desc",
        )?;
        let rows = stmt
            .query_map([], |row| {
                let kind: String = row.get(2)?;
                let target = if kind == "sqlite" {
                    Target::Sqlite { path: row.get(8)? }
                } else {
                    Target::Postgres {
                        host: row.get(3)?,
                        port: row.get::<_, Option<i64>>(4)?.map(|p| p as u16),
                        database: row.get(5)?,
                        user: row.get(6)?,
                        ssl: row.get(7)?,
                    }
                };
                Ok(SavedConnection {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    target,
                    environment: Environment::parse(&row.get::<_, String>(9)?),
                    read_only: row.get(10)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Insert (`id` 0) or update a saved connection; returns its id. Saving a
    /// SQLite file that is already saved updates that entry instead of adding one.
    pub fn save_connection(&self, saved: &SavedConnection) -> Result<i64> {
        let conn = self.lock();
        let (kind, host, port, database, user, ssl, path) = match &saved.target {
            Target::Sqlite { path } => ("sqlite", "", None, "", "", "prefer", path.as_str()),
            Target::Postgres {
                host,
                port,
                database,
                user,
                ssl,
            } => (
                "postgres",
                host.as_str(),
                port.map(i64::from),
                database.as_str(),
                user.as_str(),
                ssl.as_str(),
                "",
            ),
        };
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
                     (name, kind, host, port, database, user, ssl, path, environment, read_only, last_used)
                 values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![saved.name, kind, host, port, database, user, ssl, path, environment, saved.read_only, next_stamp(&conn)?],
            )?;
            Ok(conn.last_insert_rowid())
        } else {
            conn.execute(
                "update connections set name = ?1, kind = ?2, host = ?3, port = ?4, database = ?5,
                     user = ?6, ssl = ?7, path = ?8, environment = ?9, read_only = ?10, last_used = ?11
                 where id = ?12",
                params![saved.name, kind, host, port, database, user, ssl, path, environment, saved.read_only, next_stamp(&conn)?, id],
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

    /// Newest first. `filter` matches anywhere in the SQL, case-insensitively.
    pub fn history(
        &self,
        connection: &str,
        filter: &str,
        limit: usize,
    ) -> Result<Vec<HistoryEntry>> {
        let pattern = format!(
            "%{}%",
            filter
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
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

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn saves_updates_and_deletes_connections() {
        let store = Store::open_in_memory().unwrap();
        let pg = SavedConnection {
            id: 0,
            name: "acme prod".into(),
            target: Target::Postgres {
                host: "db.acme.dev".into(),
                port: Some(5432),
                database: "acme".into(),
                user: "app".into(),
                ssl: "require".into(),
            },
            environment: Environment::Production,
            read_only: true,
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
}
