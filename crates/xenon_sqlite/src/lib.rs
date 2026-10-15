//! SQLite driver, built on `rusqlite` with a bundled SQLite.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context as _, Result, anyhow};
use rusqlite::fallible_iterator::FallibleIterator as _;
use rusqlite::types::ValueRef;
use rusqlite::{Batch, InterruptHandle, OpenFlags};
use xenon_driver::{
    CancelHandle, Column, ColumnInfo, Connection, Feed, ForeignKey, IndexInfo, ObjectColumn,
    ObjectKind, Progress, QueryError, QueryOutcome, QueryResult, ResultSet, Schema, SchemaObject,
    TableInfo, Value, referential_action,
};

pub struct SqliteConnection {
    name: String,
    id: String,
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
    /// should be an error, not a new empty database. `read_only` opens it so
    /// SQLite itself refuses every write.
    pub fn open(path: &Path, read_only: bool) -> Result<Self> {
        let read_only_flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = if read_only {
            rusqlite::Connection::open_with_flags(path, read_only_flags)
        } else {
            rusqlite::Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .or_else(|_| rusqlite::Connection::open_with_flags(path, read_only_flags))
        }
        .with_context(|| format!("could not open {}", path.display()))?;
        // SQLite opens lazily; touch the schema so a non-database file fails here.
        conn.query_row("select count(*) from sqlite_schema", [], |_| Ok(()))
            .with_context(|| format!("{} is not a SQLite database", path.display()))?;

        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let id = std::fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .display()
            .to_string();
        Ok(Self::new(name, id, conn))
    }

    pub fn open_in_memory() -> Result<Self> {
        Ok(Self::new(
            ":memory:".into(),
            ":memory:".into(),
            rusqlite::Connection::open_in_memory()?,
        ))
    }

    fn new(name: String, id: String, conn: rusqlite::Connection) -> Self {
        let interrupt = Arc::new(Interrupt(conn.get_interrupt_handle()));
        Self {
            name,
            id,
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

    fn id(&self) -> String {
        format!("sqlite:{}", self.id)
    }

    fn server_version(&self) -> String {
        format!("sqlite {}", rusqlite::version())
    }

    fn execute_streaming(
        &self,
        sql: &str,
        row_limit: usize,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<QueryResult> {
        self.run(sql, row_limit, progress).map_err(query_error)
    }

    fn introspect(&self) -> Result<Vec<Schema>> {
        let conn = self.lock()?;
        let mut objects = Vec::new();
        // `table_list` marks the internal tables behind FTS and other virtual
        // tables as "shadow", so they can be left out.
        let mut list = conn.prepare(
            "select name, type from pragma_table_list \
             where schema = 'main' and type in ('table', 'view', 'virtual') \
               and name not like 'sqlite_%' \
             order by name collate nocase",
        )?;
        let mut columns = conn.prepare(
            "select name, type, pk, \"notnull\" from pragma_table_info(?1) order by cid",
        )?;

        let entries = list
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
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

    fn describe(&self, schema: &str, table: &str) -> Result<TableInfo> {
        let conn = self.lock()?;
        let mut info = TableInfo::default();
        // `hidden` is 2 or 3 for generated columns.
        let mut columns = conn.prepare(
            "select name, type, \"notnull\", dflt_value, pk, hidden \
             from pragma_table_xinfo(?1, ?2) where hidden != 1 order by cid",
        )?;
        info.columns = columns
            .query_map([table, schema], |row| {
                let generated = row.get::<_, i64>(5)? >= 2;
                Ok(ColumnInfo {
                    name: row.get(0)?,
                    type_name: row.get(1)?,
                    nullable: !row.get::<_, bool>(2)?,
                    default: if generated {
                        Some("generated".into())
                    } else {
                        row.get(3)?
                    },
                    primary_key: row.get::<_, i64>(4)? > 0,
                    comment: None,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;

        let mut indexes = conn.prepare(
            "select name, \"unique\", origin = 'pk' from pragma_index_list(?1, ?2) order by origin = 'pk' desc, name",
        )?;
        let mut index_columns = conn.prepare(
            "select coalesce(name, '(expression)') from pragma_index_info(?1, ?2) order by seqno",
        )?;
        let listed = indexes
            .query_map([table, schema], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, bool>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (name, unique, primary) in listed {
            let columns = index_columns
                .query_map([&name, schema], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            info.indexes.push(IndexInfo {
                name,
                columns,
                unique,
                primary,
            });
        }
        // A rowid table's `integer primary key` has no index of its own.
        if !info.indexes.iter().any(|i| i.primary) {
            let key: Vec<String> = info
                .columns
                .iter()
                .filter(|c| c.primary_key)
                .map(|c| c.name.clone())
                .collect();
            if !key.is_empty() {
                info.indexes.insert(
                    0,
                    IndexInfo {
                        name: "primary key".into(),
                        columns: key,
                        unique: true,
                        primary: true,
                    },
                );
            }
        }

        // `to` is null when a key points at the other table's primary key.
        let primary_key = |table: &str| -> rusqlite::Result<Vec<String>> {
            conn.prepare("select name from pragma_table_info(?1, ?2) where pk > 0 order by pk")?
                .query_map([table, schema], |row| row.get(0))?
                .collect()
        };
        let quoted = format!("\"{}\"", schema.replace('"', "\"\""));
        let mut keys = conn.prepare(&format!(
            "select m.name, f.id, f.\"from\", f.\"table\", f.\"to\", f.on_delete, f.on_update
             from {quoted}.sqlite_schema m
             join pragma_foreign_key_list(m.name, ?2) f
             where m.type = 'table' and (m.name = ?1 or f.\"table\" = ?1 collate nocase)
             order by m.name, f.id, f.seq"
        ))?;
        type KeyRow = (String, i64, String, String, Option<String>, String, String);
        let rows = keys
            .query_map([table, schema], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<KeyRow>>>()?;
        let mut all: Vec<(i64, ForeignKey)> = Vec::new();
        for (from_table, id, from, to_table, to, on_delete, on_update) in rows {
            match all.last_mut() {
                Some((last_id, last)) if *last_id == id && last.table == from_table => {
                    last.columns.push(from);
                    last.ref_columns.extend(to);
                }
                _ => all.push((
                    id,
                    ForeignKey {
                        name: String::new(),
                        schema: schema.into(),
                        table: from_table,
                        columns: vec![from],
                        ref_schema: schema.into(),
                        ref_table: to_table,
                        ref_columns: to.into_iter().collect(),
                        on_delete: referential_action(&on_delete),
                        on_update: referential_action(&on_update),
                    },
                )),
            }
        }
        for (_, mut key) in all {
            if key.ref_columns.is_empty() {
                key.ref_columns = primary_key(&key.ref_table)?;
            }
            key.name = format!("{}_{}_fkey", key.table, key.columns.join("_"));
            if key.ref_table.eq_ignore_ascii_case(table) {
                key.ref_table = table.into();
                info.referenced_by.push(key.clone());
            }
            if key.table == table {
                info.foreign_keys.push(key);
            }
        }
        drop(keys);

        info.definition = conn
            .query_row(
                &format!("select sql from {quoted}.sqlite_schema where name = ?1"),
                [table],
                |row| row.get(0),
            )
            .ok()
            .flatten();
        Ok(info)
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        self.interrupt.clone()
    }

    fn is_local(&self) -> bool {
        true
    }

    fn default_schema(&self) -> Option<String> {
        Some("main".into())
    }

    fn explain(&self, sql: &str) -> String {
        format!("explain query plan {sql}")
    }
}

impl SqliteConnection {
    fn run(
        &self,
        sql: &str,
        row_limit: usize,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<QueryResult> {
        let conn = self.lock()?;
        let started = Instant::now();
        let mut feed = Feed::new(progress);
        let writes = xenon_driver::sql::writes(sql);
        let mut cancelled = false;
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
            feed.next_set();
            let mut rows = stmt.query([])?;
            loop {
                let row = match rows.next() {
                    Ok(Some(row)) => row,
                    Ok(None) => break,
                    // Interrupted (cancel) while reading: keep the rows read.
                    Err(rusqlite::Error::SqliteFailure(err, _))
                        if err.code == rusqlite::ErrorCode::OperationInterrupted
                            && !result.rows.is_empty() =>
                    {
                        cancelled = true;
                        result.truncated = true;
                        break;
                    }
                    Err(err) => return Err(err.into()),
                };
                if result.rows.len() == row_limit {
                    result.truncated = true;
                    // A write's rows are all read, so it surely finishes.
                    if writes {
                        continue;
                    }
                    break;
                }
                let mut values = Vec::with_capacity(width);
                for ix in 0..width {
                    values.push(to_value(row.get_ref(ix)?));
                }
                result.rows.push(values);
                feed.read(&result, |_| {});
            }
            outcome = Some(QueryOutcome::Rows(result));
            if cancelled {
                break;
            }
        }

        Ok(QueryResult {
            outcome: outcome.ok_or_else(|| anyhow!("nothing to run"))?,
            statements,
            elapsed: started.elapsed(),
            cancelled,
        })
    }
}

/// SQLite's own message as a [`QueryError`], like the other engines' errors.
fn query_error(err: anyhow::Error) -> anyhow::Error {
    match err.downcast::<rusqlite::Error>() {
        Ok(rusqlite::Error::SqliteFailure(_, Some(message))) => QueryError {
            message,
            position: None,
        }
        .into(),
        Ok(other) => QueryError {
            message: other.to_string(),
            position: None,
        }
        .into(),
        Err(err) => err,
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
    fn describe_lists_columns_indexes_and_keys_both_ways() {
        let db = SqliteConnection::open_in_memory().unwrap();
        db.execute(
            "create table customers (id integer primary key, email text not null unique);
             create table orders (
                 id integer primary key,
                 customer_id integer references customers on delete cascade,
                 status text default 'new',
                 total real generated always as (1.0) virtual
             );
             create index orders_status on orders (status, customer_id);",
            10,
        )
        .unwrap();
        let orders = db.describe("main", "orders").unwrap();
        let names: Vec<_> = orders.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["id", "customer_id", "status", "total"]);
        assert!(orders.columns[0].primary_key);
        assert_eq!(orders.columns[2].default.as_deref(), Some("'new'"));
        assert_eq!(orders.columns[3].default.as_deref(), Some("generated"));
        assert_eq!(orders.indexes[0].columns, ["id"]);
        assert!(orders.indexes[0].primary);
        assert_eq!(orders.indexes[1].columns, ["status", "customer_id"]);
        let key = &orders.foreign_keys[0];
        assert_eq!(key.single(), Some(("customer_id", "id")));
        assert_eq!(key.ref_table, "customers");
        assert_eq!(key.on_delete.as_deref(), Some("cascade"));
        assert!(
            orders
                .definition
                .unwrap()
                .starts_with("CREATE TABLE orders")
        );

        let customers = db.describe("main", "customers").unwrap();
        assert!(customers.foreign_keys.is_empty());
        assert_eq!(customers.referenced_by.len(), 1);
        assert_eq!(customers.referenced_by[0].table, "orders");
        assert!(
            customers
                .indexes
                .iter()
                .any(|i| i.unique && i.columns == ["email"])
        );
    }

    #[test]
    fn conformance() {
        let db = SqliteConnection::open_in_memory().unwrap();
        xenon_driver::conformance::run(
            &db,
            &xenon_driver::conformance::Dialect {
                integer: "integer",
                text: "text",
                decimal: "numeric",
                real: "real",
                blob: "blob",
                series: |n| {
                    format!(
                        "with recursive s(i) as (select 1 union all select i + 1 from s where i < {n}) select i from s"
                    )
                },
                slow: "with recursive c(x) as (select 1 union all select x + 1 from c where x < 1000000000) \
                       select count(*) from c",
                rollback: "rollback",
                referential_actions: true,
                delete_returning: Some("delete from {table} returning id"),
            },
        );
    }

    #[test]
    fn streaming_previews_first_rows_then_counts() {
        let db = SqliteConnection::open_in_memory().unwrap();
        let sql = "with recursive n(i) as (select 1 union all select i + 1 from n where i < 5000) select i from n";
        let mut previews = Vec::new();
        let mut counts = Vec::new();
        let result = db
            .execute_streaming(sql, 100_000, &mut |progress| match progress {
                xenon_driver::Progress::Preview(rows) => previews.push(rows.rows.len()),
                xenon_driver::Progress::Rows(n) => counts.push(n),
            })
            .unwrap();
        assert_eq!(previews, vec![xenon_driver::PREVIEW_ROWS]);
        assert!(counts.contains(&4_000));
        assert!(!result.cancelled);
        assert_eq!(rows(result).rows.len(), 5000);
    }

    #[test]
    fn select_returns_typed_values_and_decl_types() {
        let result = rows(
            db().execute("select * from users order by id", 100)
                .unwrap(),
        );
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
            .execute(
                "update users set score = 2; select count(*) from users",
                100,
            )
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
    fn introspect_hides_full_text_search_internals() {
        let db = db();
        db.execute("create virtual table notes_fts using fts5(body)", 10)
            .unwrap();
        let names: Vec<String> = db.introspect().unwrap()[0]
            .objects
            .iter()
            .map(|o| o.name.clone())
            .collect();
        assert!(names.contains(&"notes_fts".to_string()), "{names:?}");
        assert!(
            !names.iter().any(|n| n.starts_with("notes_fts_")),
            "{names:?}"
        );
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
        assert!(SqliteConnection::open(&dir.join("missing.db"), false).is_err());
        let junk = dir.join("junk.db");
        std::fs::write(&junk, b"this is not a database at all, just some text").unwrap();
        assert!(SqliteConnection::open(&junk, false).is_err());

        let real = dir.join("real.db");
        rusqlite::Connection::open(&real)
            .unwrap()
            .execute_batch("create table t (x int)")
            .unwrap();
        let read_only = SqliteConnection::open(&real, true).unwrap();
        assert!(read_only.execute("select * from t", 10).is_ok());
        let err = read_only
            .execute("insert into t values (1)", 10)
            .unwrap_err();
        assert!(err.to_string().contains("readonly"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn explain_returns_plan_rows() {
        let db = db();
        let result = rows(
            db.execute(&db.explain("select * from users where id = 1"), 10)
                .unwrap(),
        );
        assert!(result.columns.iter().any(|c| c.name == "detail"));
        assert!(!result.rows.is_empty());
    }

    #[test]
    fn quote_ident_escapes_quotes() {
        assert_eq!(db().quote_ident("we\"ird"), "\"we\"\"ird\"");
    }
}
