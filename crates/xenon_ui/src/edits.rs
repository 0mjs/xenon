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

    let mut out = Vec::new();
    for &row in &edits.deleted {
        out.push(Statement {
            sql: format!("delete from {name} where {}", key_of(row)),
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
        out.push(Statement {
            sql: format!("update {name} set {assignments} where {}", key_of(row)),
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
                    "a row was changed or deleted after it was loaded".to_string()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use xenon_sqlite::SqliteConnection;

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
                "delete from orders where id = 1",
                "update orders set status = 'o''clock', note = NULL where id = 2",
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
}

/// Saving edits on every server engine in the test fleet:
/// `dev/fleet/up.sh postgres mysql mariadb mssql`, then
/// `cargo test -p xenon_ui edits::live -- --ignored`.
#[cfg(test)]
mod live {
    use super::*;
    use xenon_driver::ConnectOptions;
    use xenon_store::ServerEngine;

    fn fleet(engine: ServerEngine, port: u16) -> std::sync::Arc<dyn Connection> {
        let (user, password) = match engine {
            ServerEngine::SqlServer => ("sa", "Xenon-fleet-1"),
            _ => ("xenon", "xenon"),
        };
        crate::servers::connect(
            engine,
            &ConnectOptions {
                host: "127.0.0.1".into(),
                port: Some(port),
                database: "shop".into(),
                user: user.into(),
                password: password.into(),
                ..Default::default()
            },
        )
        .expect("is the fleet up?")
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

    #[test]
    #[ignore]
    fn postgres() {
        check(ServerEngine::Postgres, 55432);
    }

    #[test]
    #[ignore]
    fn mysql() {
        check(ServerEngine::Mysql, 53306);
    }

    #[test]
    #[ignore]
    fn mariadb() {
        check(ServerEngine::Mysql, 53307);
    }

    #[test]
    #[ignore]
    fn sql_server() {
        check(ServerEngine::SqlServer, 51433);
    }
}
