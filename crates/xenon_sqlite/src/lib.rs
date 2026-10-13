//! SQLite driver, built on `rusqlite` with a bundled SQLite.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context as _, Result, anyhow};
use rusqlite::fallible_iterator::FallibleIterator as _;
use rusqlite::types::ValueRef;
use rusqlite::{Batch, InterruptHandle, OpenFlags};
use xenon_driver::{
    CancelHandle, Column, Connection, ObjectColumn, ObjectKind, QueryOutcome, QueryResult,
    ResultSet, Schema, SchemaObject, Value,
};

pub struct SqliteConnection {
    name: String,
    conn: Mutex<rusqlite::Connection>,
    interrupt: Arc<Interrupt>,
}

struct Interrupt(InterruptHandle);

impl CancelHandle for Interrupt {
    fn cancel(&self) {
        self.0.interrupt();
    }
}

impl SqliteConnection {
    /// Open an existing database file. Never creates one: a typo in a path
    /// should be an error, not a new empty database.
    pub fn open(path: &Path) -> Result<Self> {
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = rusqlite::Connection::open_with_flags(path, flags)
            .or_else(|_| {
                rusqlite::Connection::open_with_flags(
                    path,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )
            })
            .with_context(|| format!("could not open {}", path.display()))?;
        // SQLite opens lazily; touch the schema so a non-database file fails here.
        conn.query_row("select count(*) from sqlite_schema", [], |_| Ok(()))
            .with_context(|| format!("{} is not a SQLite database", path.display()))?;

        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Ok(Self::new(name, conn))
    }

    pub fn open_in_memory() -> Result<Self> {
        Ok(Self::new(
            ":memory:".into(),
            rusqlite::Connection::open_in_memory()?,
        ))
    }

    fn new(name: String, conn: rusqlite::Connection) -> Self {
        let interrupt = Arc::new(Interrupt(conn.get_interrupt_handle()));
        Self {
            name,
            conn: Mutex::new(conn),
            interrupt,
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, rusqlite::Connection>> {
        self.conn
            .lock()
            .map_err(|_| anyhow!("connection is unusable after a crash in a previous query"))
    }
}

impl Connection for SqliteConnection {
    fn name(&self) -> &str {
        &self.name
    }

    fn server_version(&self) -> String {
        format!("sqlite {}", rusqlite::version())
    }

    fn execute(&self, sql: &str, row_limit: usize) -> Result<QueryResult> {
        let conn = self.lock()?;
        let started = Instant::now();
        let mut batch = Batch::new(&conn, sql);
        let mut outcome = None;
        let mut statements = 0;

        while let Some(mut stmt) = batch.next()? {
            statements += 1;
            if stmt.column_count() == 0 {
                outcome = Some(QueryOutcome::Affected(stmt.execute([])? as u64));
                continue;
            }

            let columns: Vec<Column> = stmt
                .columns()
                .iter()
                .map(|c| Column {
                    name: c.name().to_string(),
                    type_name: c.decl_type().map(str::to_string),
                })
                .collect();
            let width = columns.len();
            let mut result = ResultSet {
                columns,
                ..Default::default()
            };
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                if result.rows.len() == row_limit {
                    result.truncated = true;
                    break;
                }
                let mut values = Vec::with_capacity(width);
                for ix in 0..width {
                    values.push(to_value(row.get_ref(ix)?));
                }
                result.rows.push(values);
            }
            outcome = Some(QueryOutcome::Rows(result));
        }

        Ok(QueryResult {
            outcome: outcome.ok_or_else(|| anyhow!("nothing to run"))?,
            statements,
            elapsed: started.elapsed(),
        })
    }

    fn introspect(&self) -> Result<Vec<Schema>> {
        let conn = self.lock()?;
        let mut objects = Vec::new();
        let mut list = conn.prepare(
            "select name, type from sqlite_schema \
             where type in ('table', 'view') and name not like 'sqlite_%' \
             order by name collate nocase",
        )?;
        let mut columns = conn.prepare(
            "select name, type, pk, \"notnull\" from pragma_table_info(?1) order by cid",
        )?;

        let entries = list
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (name, kind) in entries {
            let cols = columns
                .query_map([&name], |row| {
                    Ok(ObjectColumn {
                        name: row.get(0)?,
                        type_name: row.get(1)?,
                        primary_key: row.get::<_, i64>(2)? > 0,
                        not_null: row.get(3)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            objects.push(SchemaObject {
                name,
                kind: if kind == "view" {
                    ObjectKind::View
                } else {
                    ObjectKind::Table
                },
                columns: cols,
            });
        }

        Ok(vec![Schema {
            name: "main".into(),
            objects,
        }])
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        self.interrupt.clone()
    }

    fn is_local(&self) -> bool {
        true
    }
}

fn to_value(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(v) => Value::Integer(v),
        ValueRef::Real(v) => Value::Real(v),
        ValueRef::Text(v) => Value::Text(String::from_utf8_lossy(v).into_owned()),
        ValueRef::Blob(v) => Value::Blob(v.to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> SqliteConnection {
        let db = SqliteConnection::open_in_memory().unwrap();
        db.execute(
            "create table users (id integer primary key, email text not null, score real, avatar blob);
             insert into users values (1, 'a@x.io', 1.5, null), (2, 'b@x.io', null, x'0102');
             create view emails as select email from users;",
            100,
        )
        .unwrap();
        db
    }

    fn rows(result: QueryResult) -> ResultSet {
        match result.outcome {
            QueryOutcome::Rows(rows) => rows,
            other => panic!("expected rows, got {other:?}"),
        }
    }

    #[test]
    fn select_returns_typed_values_and_decl_types() {
        let result = rows(db().execute("select * from users order by id", 100).unwrap());
        assert_eq!(result.columns[1].name, "email");
        assert_eq!(result.columns[0].type_name.as_deref(), Some("INTEGER"));
        assert_eq!(result.rows[0][1], Value::Text("a@x.io".into()));
        assert_eq!(result.rows[1][2], Value::Null);
        assert_eq!(result.rows[1][3], Value::Blob(vec![1, 2]));
        assert!(!result.truncated);
    }

    #[test]
    fn multiple_statements_return_the_last_outcome() {
        let result = db()
            .execute("update users set score = 2; select count(*) from users", 100)
            .unwrap();
        assert_eq!(result.statements, 2);
        assert_eq!(rows(result).rows[0][0], Value::Integer(2));
    }

    #[test]
    fn non_query_reports_affected_rows() {
        let result = db().execute("delete from users where id = 2", 100).unwrap();
        assert!(matches!(result.outcome, QueryOutcome::Affected(1)));
    }

    #[test]
    fn row_limit_truncates() {
        let result = rows(db().execute("select * from users", 1).unwrap());
        assert_eq!(result.rows.len(), 1);
        assert!(result.truncated);
    }

    #[test]
    fn errors_carry_sqlite_message() {
        let err = db().execute("select emial from users", 100).unwrap_err();
        assert!(err.to_string().contains("no such column: emial"), "{err}");
    }

    #[test]
    fn empty_input_is_an_error() {
        assert!(db().execute("  -- just a comment\n", 100).is_err());
    }

    #[test]
    fn introspect_lists_tables_views_and_columns() {
        let schemas = db().introspect().unwrap();
        let objects = &schemas[0].objects;
        assert_eq!(objects.len(), 2);
        assert_eq!(objects[0].name, "emails");
        assert_eq!(objects[0].kind, ObjectKind::View);
        let users = &objects[1];
        assert_eq!(users.kind, ObjectKind::Table);
        assert!(users.columns[0].primary_key);
        assert!(users.columns[1].not_null);
        assert_eq!(users.columns[2].type_name, "REAL");
    }

    #[test]
    fn open_rejects_missing_and_non_database_files() {
        let dir = std::env::temp_dir().join(format!("xenon-sqlite-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(SqliteConnection::open(&dir.join("missing.db")).is_err());
        let junk = dir.join("junk.db");
        std::fs::write(&junk, b"this is not a database at all, just some text").unwrap();
        assert!(SqliteConnection::open(&junk).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn quote_ident_escapes_quotes() {
        assert_eq!(db().quote_ident("we\"ird"), "\"we\"\"ird\"");
    }
}
