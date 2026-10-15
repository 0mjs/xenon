//! Changes to a table's rows, staged in the grid and saved together: the
//! statements they become, and saving them in one transaction.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, anyhow};
use xenon_driver::{Connection, QueryOutcome, ResultSet, Value};

/// A row in an editable grid: one fetched from the table, or one being added.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RowRef {
    /// Index into the result's rows (not the display order).
    Existing(usize),
    /// Index into [`Edits::inserted`].
    New(usize),
}

/// Staged changes to one table, keyed by data row so sorting the grid
/// doesn't lose them.
#[derive(Clone, Debug, Default)]
pub struct Edits {
    /// New values for cells of existing rows, by (row, column).
    pub cells: BTreeMap<(usize, usize), Value>,
    /// Existing rows to delete.
    pub deleted: BTreeSet<usize>,
    /// Rows to add: a value per column, or `None` to let the database fill it
    /// in (its default, an identity, a sequence).
    pub inserted: Vec<Vec<Option<Value>>>,
}

impl Edits {
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty() && self.deleted.is_empty() && self.inserted.is_empty()
    }

    /// Rows touched: each edited, deleted or added row counts once.
    pub fn count(&self) -> usize {
        let edited: BTreeSet<usize> = self
            .cells
            .keys()
            .map(|(row, _)| *row)
            .filter(|row| !self.deleted.contains(row))
            .collect();
        edited.len() + self.deleted.len() + self.inserted.len()
    }

    /// Stage a new value for a cell.
    pub fn set(&mut self, row: RowRef, column: usize, value: Value, original: Option<&Value>) {
        match row {
            RowRef::Existing(row) => {
                // Setting a cell back to what it was un-stages it.
                if original == Some(&value) {
                    self.cells.remove(&(row, column));
                } else {
                    self.cells.insert((row, column), value);
                }
            }
            RowRef::New(row) => {
                if let Some(cell) = self.inserted.get_mut(row).and_then(|r| r.get_mut(column)) {
                    *cell = Some(value);
                }
            }
        }
    }

    /// The value a cell will have once saved, if it's been changed.
    pub fn value(&self, row: RowRef, column: usize) -> Option<Option<&Value>> {
        match row {
            RowRef::Existing(row) => self.cells.get(&(row, column)).map(Some),
            RowRef::New(row) => self
                .inserted
                .get(row)
                .map(|r| r.get(column).and_then(Option::as_ref)),
        }
    }
}

/// The table the grid shows, and how to find its rows again.
#[derive(Clone, Debug)]
pub struct TableRef {
    /// `None` for the connection's default schema.
    pub schema: Option<String>,
    pub table: String,
    /// Result columns that make up the primary key.
    pub key: Vec<usize>,
}

/// One statement to save, and whether it must touch exactly one row.
#[derive(Clone, Debug, PartialEq)]
pub struct Statement {
    pub sql: String,
    pub exactly_one: bool,
}

/// Whether a column's loaded value can be checked with `=` before saving:
/// not JSON (Postgres's `json` has no `=`), XML, spatial or SQL Server's legacy
/// `text`/`ntext`/`image`.
fn comparable(conn: &dyn Connection, column: &xenon_driver::Column) -> bool {
    let kind = column
        .type_name
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();
    let legacy = conn.uses_top() && matches!(kind.as_str(), "text" | "ntext" | "image");
    !(legacy
        || kind.contains("json")
        || kind.contains("xml")
        || kind.contains("geometry")
        || kind.contains("geography"))
}

/// The statements that save `edits`: deletes, then updates, then inserts.
pub fn statements(
    conn: &dyn Connection,
    target: &TableRef,
    data: &ResultSet,
    edits: &Edits,
) -> Vec<Statement> {
    let name = match &target.schema {
        Some(schema) => format!("{}.{}", conn.ident(schema), conn.ident(&target.table)),
        None => conn.ident(&target.table),
    };
    let column = |ix: usize| conn.ident(&data.columns[ix].name);
    // Rows are found by their key as fetched, even when the key itself is edited.
    let key_of = |row: usize| {
        target
            .key
            .iter()
            .map(|&ix| match &data.rows[row][ix] {
                Value::Null => format!("{} is null", column(ix)),
                value => format!("{} = {}", column(ix), conn.literal(value)),
            })
            .collect::<Vec<_>>()
            .join(" and ")
    };

    // `and col = <loaded value>` for each of `columns` that can be compared: the
    // statement then matches nothing if someone else changed the row meanwhile,
    // and the save stops instead of overwriting or deleting their change.
    let unchanged = |row: usize, columns: &mut dyn Iterator<Item = usize>| -> String {
        columns
            .filter(|col| !target.key.contains(col) && comparable(conn, &data.columns[*col]))
            .map(|col| match &data.rows[row][col] {
                Value::Null => format!(" and {} is null", column(col)),
                Value::Real(_) => String::new(),
                old => format!(" and {} = {}", column(col), conn.literal(old)),
            })
            .collect()
    };

    let mut out = Vec::new();
    for &row in &edits.deleted {
        let check = unchanged(row, &mut (0..data.columns.len()));
        out.push(Statement {
            sql: format!("delete from {name} where {}{check}", key_of(row)),
            exactly_one: true,
        });
    }
    let mut updates: BTreeMap<usize, Vec<(usize, &Value)>> = BTreeMap::new();
    for ((row, col), value) in &edits.cells {
        if !edits.deleted.contains(row) {
            updates.entry(*row).or_default().push((*col, value));
        }
    }
    for (row, cells) in updates {
        let assignments = cells
            .iter()
            .map(|(col, value)| format!("{} = {}", column(*col), conn.literal(value)))
            .collect::<Vec<_>>()
            .join(", ");
        // Only if the edited cells still hold what was loaded.
        let unchanged = unchanged(row, &mut cells.iter().map(|(col, _)| *col));
        out.push(Statement {
            sql: format!(
                "update {name} set {assignments} where {}{unchanged}",
                key_of(row)
            ),
            exactly_one: true,
        });
    }
    for row in &edits.inserted {
        let given: Vec<(usize, &Value)> = row
            .iter()
            .enumerate()
            .filter_map(|(ix, value)| value.as_ref().map(|v| (ix, v)))
            .collect();
        let sql = if given.is_empty() {
            format!("insert into {name} default values")
        } else {
            format!(
                "insert into {name} ({}) values ({})",
                given
                    .iter()
                    .map(|(ix, _)| column(*ix))
                    .collect::<Vec<_>>()
                    .join(", "),
                given
                    .iter()
                    .map(|(_, v)| conn.literal(v))
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        };
        out.push(Statement {
            sql,
            exactly_one: false,
        });
    }
    out
}

/// The statements as one script, for review and for the history.
pub fn script(conn: &dyn Connection, statements: &[Statement]) -> String {
    let mut lines = vec![format!("{};", conn.begin())];
    lines.extend(statements.iter().map(|s| format!("{};", s.sql)));
    lines.push("commit;".into());
    lines.join("\n")
}

/// Run `statements` in one transaction. Updates and deletes must each touch
/// exactly one row; if any doesn't (someone else changed or deleted it), or
/// anything fails, nothing is saved. Blocking: call off the main thread.
pub fn save(conn: &dyn Connection, statements: &[Statement]) -> Result<usize> {
    conn.execute(conn.begin(), 0)?;
    let run = || -> Result<usize> {
        let mut rows = 0;
        for statement in statements {
            let outcome = conn.execute(&statement.sql, 0)?.outcome;
            let affected = match outcome {
                QueryOutcome::Affected(n) => n,
                QueryOutcome::Rows(_) => 0,
            };
            if statement.exactly_one && affected != 1 {
                let why = if affected == 0 {
                    "a row was changed or deleted after it was loaded (reload the table to see it now)"
                        .to_string()
                } else {
                    format!("its key matched {affected} rows, not one")
                };
                return Err(anyhow!("Nothing was saved: {why}.\n\n{}", statement.sql));
            }
            rows += affected as usize;
        }
        Ok(rows)
    };
    match run() {
        Ok(rows) => {
            conn.execute("commit", 0)?;
            Ok(rows)
        }
        Err(err) => {
            // The original error matters more than a failed rollback.
            let _ = conn.execute("rollback", 0);
            Err(err)
        }
    }
}

/// [`save`], stopped and rolled back if it's still running after `limit` (the
/// production time limit), so a save waiting behind someone's locks can't
/// hang on.
pub fn save_within(
    conn: &dyn Connection,
    statements: &[Statement],
    limit: Option<std::time::Duration>,
) -> Result<usize> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let Some(limit) = limit else {
        return save(conn, statements);
    };
    let (done, timed_out) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let cancel = conn.cancel_handle();
    {
        let (done, timed_out) = (done.clone(), timed_out.clone());
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + limit;
            while !done.load(Ordering::SeqCst) {
                if std::time::Instant::now() >= deadline {
                    timed_out.store(true, Ordering::SeqCst);
                    cancel.cancel();
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        });
    }
    let saved = save(conn, statements);
    done.store(true, Ordering::SeqCst);
    match saved {
        Err(_) if timed_out.load(Ordering::SeqCst) => Err(anyhow!(
            "Nothing was saved: the save was still running after the production time limit \
             ({} s, Settings → Safety), so Xenon stopped it and rolled it back.",
            limit.as_secs()
        )),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use xenon_sqlite::SqliteConnection;

    #[test]
    fn a_save_past_the_time_limit_is_rolled_back() {
        let (db, path) = shop();
        // A trigger that takes seconds: the save is still running at the limit.
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(
                "create trigger slow after update on orders begin
                     select count(*) from (with recursive c(x) as
                         (select 1 union all select x + 1 from c where x < 300000000) select x from c);
                 end;",
            )
            .unwrap();
        let statements = [Statement {
            sql: "update orders set status = 'lost' where id = 1".into(),
            exactly_one: true,
        }];
        let started = std::time::Instant::now();
        let err = save_within(
            &db,
            &statements,
            Some(std::time::Duration::from_millis(300)),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("time limit"), "{err}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        // Rolled back, and the connection still works.
        let status = match db
            .execute("select status from orders where id = 1", 1)
            .unwrap()
            .outcome
        {
            QueryOutcome::Rows(rows) => rows.rows[0][0].clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(status, Value::Text("paid".into()));
        // Without a limit (or within it), it's a plain save.
        let quick = [Statement {
            sql: "update orders set note = 'quick' where id = 2".into(),
            exactly_one: true,
        }];
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch("drop trigger slow")
            .unwrap();
        assert_eq!(
            save_within(&db, &quick, Some(std::time::Duration::from_secs(5))).unwrap(),
            1
        );
    }

    fn shop() -> (SqliteConnection, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "xenon-edits-{}-{}.db",
            std::process::id(),
            rand_suffix()
        ));
        let _ = std::fs::remove_file(&path);
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(
            "create table orders (id integer primary key, status text not null, total numeric, note text default 'none');
             insert into orders values (1, 'paid', 10.5, null), (2, 'pending', 3, 'it''s'), (3, 'paid', 7, null);",
        )
        .unwrap();
        (
            SqliteConnection::open(Path::new(&path), false).unwrap(),
            path,
        )
    }

    fn rand_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }

    fn fetch(conn: &SqliteConnection) -> ResultSet {
        match conn
            .execute("select * from orders order by id", 100)
            .unwrap()
            .outcome
        {
            QueryOutcome::Rows(rows) => rows,
            other => panic!("{other:?}"),
        }
    }

    fn target() -> TableRef {
        TableRef {
            schema: None,
            table: "orders".into(),
            key: vec![0],
        }
    }

    #[test]
    fn staging_and_counting() {
        let mut edits = Edits::default();
        let paid = Value::Text("paid".into());
        edits.set(
            RowRef::Existing(0),
            1,
            Value::Text("refunded".into()),
            Some(&paid),
        );
        edits.set(
            RowRef::Existing(0),
            2,
            Value::Integer(9),
            Some(&Value::Integer(10)),
        );
        edits.deleted.insert(2);
        edits.inserted.push(vec![None; 4]);
        assert_eq!(edits.count(), 3);
        // Back to the original value: no longer a change.
        edits.set(RowRef::Existing(0), 1, paid.clone(), Some(&paid));
        assert_eq!(edits.cells.len(), 1);
    }

    #[test]
    fn statements_read_like_sql() {
        let (conn, path) = shop();
        let data = fetch(&conn);
        let mut edits = Edits::default();
        edits.set(RowRef::Existing(1), 1, Value::Text("o'clock".into()), None);
        edits.set(RowRef::Existing(1), 3, Value::Null, None);
        edits.deleted.insert(0);
        let mut new = vec![None; 4];
        new[1] = Some(Value::Text("pending".into()));
        edits.inserted.push(new);
        let sql: Vec<String> = statements(&conn, &target(), &data, &edits)
            .into_iter()
            .map(|s| s.sql)
            .collect();
        assert_eq!(
            sql,
            [
                // Only if the row is as it was loaded.
                "delete from orders where id = 1 and status = 'paid' and note is null",
                // Saved only if the edited cells still hold what was loaded.
                "update orders set status = 'o''clock', note = NULL \
                 where id = 2 and status = 'pending' and note = 'it''s'",
                "insert into orders (status) values ('pending')",
            ]
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn saving_applies_everything_in_one_transaction() {
        let (conn, path) = shop();
        let data = fetch(&conn);
        let mut edits = Edits::default();
        edits.set(RowRef::Existing(1), 1, Value::Text("paid".into()), None);
        edits.deleted.insert(2);
        let mut new = vec![None; 4];
        new[1] = Some(Value::Text("new".into()));
        edits.inserted.push(new);
        let statements = statements(&conn, &target(), &data, &edits);
        assert_eq!(save(&conn, &statements).unwrap(), 3);
        let after = fetch(&conn);
        assert_eq!(after.rows.len(), 3);
        assert_eq!(after.rows[1][1], Value::Text("paid".into()));
        // The new row got the column's default.
        assert_eq!(after.rows[2][3], Value::Text("none".into()));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn a_row_changed_elsewhere_saves_nothing() {
        let (conn, path) = shop();
        let data = fetch(&conn);
        let mut edits = Edits::default();
        edits.set(RowRef::Existing(0), 1, Value::Text("refunded".into()), None);
        edits.set(RowRef::Existing(2), 1, Value::Text("refunded".into()), None);
        // Someone else deletes row 3 after we loaded it.
        conn.execute("delete from orders where id = 3", 0).unwrap();
        let err = save(&conn, &statements(&conn, &target(), &data, &edits)).unwrap_err();
        assert!(err.to_string().starts_with("Nothing was saved"), "{err}");
        // Row 1's update was rolled back with it.
        assert_eq!(fetch(&conn).rows[0][1], Value::Text("paid".into()));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn a_delete_never_removes_a_row_someone_else_changed() {
        let (conn, path) = shop();
        let data = fetch(&conn);
        let mut edits = Edits::default();
        edits.deleted.insert(0);
        // Someone else updates the row after we loaded it.
        conn.execute("update orders set note = 'keep me' where id = 1", 0)
            .unwrap();
        let err = save(&conn, &statements(&conn, &target(), &data, &edits)).unwrap_err();
        assert!(err.to_string().starts_with("Nothing was saved"), "{err}");
        assert_eq!(fetch(&conn).rows.len(), data.rows.len());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn saving_works_on_duckdb_too() {
        let conn = xenon_duckdb::DuckDbConnection::open_in_memory().unwrap();
        conn.execute(
            "create table orders (id integer primary key, status varchar, note varchar, total double);
             insert into orders values (1, 'paid', null, 1.5), (2, 'pending', 'x', 2.5)",
            10,
        )
        .unwrap();
        let data = match conn
            .execute("select id, status, note, total from orders order by id", 10)
            .unwrap()
            .outcome
        {
            QueryOutcome::Rows(rows) => rows,
            other => panic!("{other:?}"),
        };
        let mut edits = Edits::default();
        edits.set(RowRef::Existing(0), 1, Value::Text("refunded".into()), None);
        edits.deleted.insert(1);
        let target = TableRef {
            schema: None,
            table: "orders".into(),
            key: vec![0],
        };
        assert_eq!(
            save(&conn, &statements(&conn, &target, &data, &edits)).unwrap(),
            2
        );
        let after = match conn
            .execute("select status from orders", 10)
            .unwrap()
            .outcome
        {
            QueryOutcome::Rows(rows) => rows,
            other => panic!("{other:?}"),
        };
        assert_eq!(after.rows, [[Value::Text("refunded".into())]]);
    }

    #[test]
    fn an_edit_never_overwrites_someone_elses_change() {
        let (conn, path) = shop();
        let data = fetch(&conn);
        let mut edits = Edits::default();
        edits.set(RowRef::Existing(0), 1, Value::Text("refunded".into()), None);
        // Someone else changes the same cell after we loaded it.
        conn.execute("update orders set status = 'shipped' where id = 1", 0)
            .unwrap();
        let err = save(&conn, &statements(&conn, &target(), &data, &edits)).unwrap_err();
        assert!(err.to_string().starts_with("Nothing was saved"), "{err}");
        assert_eq!(fetch(&conn).rows[0][1], Value::Text("shipped".into()));
        std::fs::remove_file(path).ok();
    }
}

/// Saving edits on every server engine in the test fleet:
/// `dev/fleet/up.sh postgres mysql mariadb mssql`, then
/// `cargo test -p xenon_edit -- --ignored`.
#[cfg(test)]
mod live {
    use super::*;
    use xenon_driver::ConnectOptions;

    #[derive(Clone, Copy, Debug)]
    enum ServerEngine {
        Postgres,
        Mysql,
        SqlServer,
    }

    impl ServerEngine {
        fn as_str(self) -> &'static str {
            match self {
                ServerEngine::Postgres => "postgres",
                ServerEngine::Mysql => "mysql",
                ServerEngine::SqlServer => "sqlserver",
            }
        }
    }

    fn fleet(engine: ServerEngine, port: u16) -> Box<dyn Connection> {
        let (user, password) = match engine {
            ServerEngine::SqlServer => ("sa", "Xenon-fleet-1"),
            _ => ("xenon", "xenon"),
        };
        let options = ConnectOptions {
            host: "127.0.0.1".into(),
            port: Some(port),
            database: "shop".into(),
            user: user.into(),
            password: password.into(),
            ..Default::default()
        };
        let conn: anyhow::Result<Box<dyn Connection>> = match engine {
            ServerEngine::Postgres => xenon_postgres::PostgresConnection::connect_options(&options)
                .map(|c| Box::new(c) as _),
            ServerEngine::Mysql => {
                xenon_mysql::MysqlConnection::connect_options(&options).map(|c| Box::new(c) as _)
            }
            ServerEngine::SqlServer => {
                xenon_mssql::MssqlConnection::connect_options(&options).map(|c| Box::new(c) as _)
            }
        };
        conn.expect("is the fleet up?")
    }

    fn rows(conn: &dyn Connection, sql: &str) -> ResultSet {
        match conn.execute(sql, 1000).unwrap().outcome {
            QueryOutcome::Rows(rows) => rows,
            other => panic!("{other:?}"),
        }
    }

    fn check(engine: ServerEngine, port: u16) {
        let conn = fleet(engine, port);
        let conn = conn.as_ref();
        let table = format!("xenon_edits_{}", engine.as_str());
        let _ = conn.execute(&format!("drop table {table}"), 0);
        conn.execute(
            &format!("create table {table} (id int primary key, name varchar(50), qty int)"),
            0,
        )
        .unwrap();
        conn.execute(
            &format!("insert into {table} values (1, 'one', 1), (2, 'two''s', 2), (3, 'three', 3)"),
            0,
        )
        .unwrap();
        let select = format!("select * from {table} order by id");
        let data = rows(conn, &select);
        let target = TableRef {
            schema: None,
            table: table.clone(),
            key: vec![0],
        };

        // Update (including a value equal to the current one, which MySQL
        // reports as zero changed rows unless asked for found rows), delete, insert.
        let mut edits = Edits::default();
        edits
            .cells
            .insert((0, 1), Value::Text("ünïcödé \\ 'quoted'".into()));
        edits.cells.insert((1, 2), Value::Decimal("2".into()));
        edits.deleted.insert(2);
        edits.inserted.push(vec![
            Some(Value::Decimal("4".into())),
            Some(Value::Text("four".into())),
            None,
        ]);
        assert_eq!(
            save(conn, &statements(conn, &target, &data, &edits)).unwrap(),
            4,
            "{engine:?}"
        );
        let after = rows(conn, &select);
        assert_eq!(after.rows.len(), 3, "{engine:?}");
        assert_eq!(
            after.rows[0][1],
            Value::Text("ünïcödé \\ 'quoted'".into()),
            "{engine:?}"
        );
        assert_eq!(after.rows[2][1], Value::Text("four".into()), "{engine:?}");
        assert!(after.rows[2][2].is_null(), "{engine:?}");

        // A row deleted elsewhere: nothing saves, including the other update.
        let mut edits = Edits::default();
        edits.cells.insert((0, 2), Value::Decimal("10".into()));
        edits.cells.insert((1, 2), Value::Decimal("20".into()));
        conn.execute(&format!("delete from {table} where id = 2"), 0)
            .unwrap();
        let err = save(conn, &statements(conn, &target, &after, &edits)).unwrap_err();
        assert!(
            err.to_string().starts_with("Nothing was saved"),
            "{engine:?}: {err}"
        );
        assert_eq!(
            rows(conn, &select).rows[0][2],
            Value::Integer(1),
            "{engine:?} rolled back"
        );

        conn.execute(&format!("drop table {table}"), 0).unwrap();
    }

    /// The live tests share the fleet: run them one at a time.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A save held up past the production time limit (by a slow trigger) is
    /// stopped and rolled back, and the connection works afterwards.
    fn time_limit(engine: ServerEngine, port: u16) {
        let conn = fleet(engine, port);
        let conn = conn.as_ref();
        let table = format!("xenon_slow_{}", engine.as_str());
        let _ = conn.execute(&format!("drop table {table}"), 0);
        conn.execute(
            &format!("create table {table} (id int primary key, n int)"),
            0,
        )
        .unwrap();
        conn.execute(&format!("insert into {table} values (1, 1)"), 0)
            .unwrap();
        let trigger = match engine {
            ServerEngine::Postgres => format!(
                "create or replace function xenon_slow() returns trigger language plpgsql as
                     $$ begin perform pg_sleep(5); return new; end $$;
                 create trigger xenon_slow before update on {table}
                     for each row execute function xenon_slow()"
            ),
            // `benchmark`, not `sleep`: a killed `sleep` returns instead of failing.
            ServerEngine::Mysql => format!(
                "create trigger xenon_slow_{port} before update on {table} for each row
                     set @xenon = benchmark(2000000000, sha2('xenon', 256))"
            ),
            ServerEngine::SqlServer => format!(
                "create trigger xenon_slow on {table} after update as waitfor delay '00:00:05'"
            ),
        };
        conn.execute(&trigger, 0).unwrap();
        let statements = [Statement {
            sql: format!("update {table} set n = 2 where id = 1"),
            exactly_one: true,
        }];
        let started = std::time::Instant::now();
        let err = save_within(
            conn,
            &statements,
            Some(std::time::Duration::from_millis(500)),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("time limit"), "{engine:?}: {err}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(4),
            "{engine:?} wasn't stopped at the limit"
        );
        let data = rows(conn, &format!("select n from {table} where id = 1"));
        assert_eq!(
            data.rows[0][0],
            Value::Integer(1),
            "{engine:?} kept the change"
        );
        conn.execute(&format!("drop table {table}"), 0).unwrap();
        if let ServerEngine::Postgres = engine {
            conn.execute("drop function xenon_slow()", 0).unwrap();
        }
    }

    #[test]
    #[ignore]
    fn postgres() {
        let _serial = serial();
        check(ServerEngine::Postgres, 55432);
        time_limit(ServerEngine::Postgres, 55432);
    }

    #[test]
    #[ignore]
    fn mysql() {
        let _serial = serial();
        check(ServerEngine::Mysql, 53306);
        time_limit(ServerEngine::Mysql, 53306);
    }

    #[test]
    #[ignore]
    fn mariadb() {
        let _serial = serial();
        check(ServerEngine::Mysql, 53307);
        time_limit(ServerEngine::Mysql, 53307);
    }

    #[test]
    #[ignore]
    fn sql_server() {
        let _serial = serial();
        check(ServerEngine::SqlServer, 51433);
        time_limit(ServerEngine::SqlServer, 51433);
    }
}
