//! Xenon's local state, kept in a SQLite file in the platform data directory
//! (`~/Library/Application Support/Xenon/xenon.db` on macOS).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow};
use rusqlite::params;

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
             create index if not exists history_by_connection on history (connection, ran_at desc);",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, rusqlite::Connection> {
        // A panic mid-write leaves SQLite consistent, so a poisoned lock is safe to reuse.
        self.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn record(&self, entry: NewHistoryEntry<'_>) -> Result<HistoryEntry> {
        let ran_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        let duration_ms = entry.duration.as_millis() as i64;
        let rows = entry.rows.map(|n| n as i64);

        let conn = self.lock();
        conn.execute(
            "insert into history (connection, sql, ran_at, duration_ms, rows, error)
             values (?1, ?2, ?3, ?4, ?5, ?6)",
            params![entry.connection, entry.sql, ran_at, duration_ms, rows, entry.error],
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

    /// Newest first. `filter` matches anywhere in the SQL, case-insensitively.
    pub fn history(&self, connection: &str, filter: &str, limit: usize) -> Result<Vec<HistoryEntry>> {
        let pattern = format!(
            "%{}%",
            filter.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
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
    fn persists_across_reopen() {
        let dir = std::env::temp_dir().join(format!("xenon-store-test-{}", std::process::id()));
        let path = dir.join("nested").join("xenon.db");
        Store::open(&path).unwrap().record(entry("a", "select 1")).unwrap();
        assert_eq!(Store::open(&path).unwrap().history("a", "", 10).unwrap().len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
