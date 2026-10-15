//! One test suite every driver must pass, run against a live connection: the
//! behaviour the app relies on (values that survive a round trip, row limits,
//! streaming, cancel, keys) checked the same way on every engine.
//!
//! Each driver crate calls [`run`] from a test with its own [`Dialect`]. Tables
//! are named `xenon_conf_*` and dropped before and after.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail, ensure};

use crate::{Connection, PREVIEW_ROWS, Progress, QueryError, QueryOutcome, ResultSet, Value};

/// How to spell the few things that differ between engines.
pub struct Dialect {
    pub integer: &'static str,
    /// A Unicode text column (`nvarchar(max)` on SQL Server).
    pub text: &'static str,
    pub decimal: &'static str,
    pub real: &'static str,
    pub blob: &'static str,
    /// A query returning one integer column `i` counting from 1 to `n`.
    pub series: fn(usize) -> String,
    /// A statement that runs for about five seconds, to cancel.
    pub slow: &'static str,
    pub rollback: &'static str,
    /// Foreign keys take `on delete cascade` (DuckDB's don't).
    pub referential_actions: bool,
    /// `delete from {table}` that returns the deleted ids, where the engine has
    /// one (`returning id`, SQL Server's `output deleted.id`); `{table}` is replaced.
    pub delete_returning: Option<&'static str>,
}

type Check = fn(&dyn Connection, &Dialect) -> Result<()>;

/// Run every check, then fail with the list of those that didn't pass.
pub fn run(conn: &dyn Connection, dialect: &Dialect) {
    let checks: [(&str, Check); 13] = [
        (
            "a read-only transaction refuses writes",
            read_only_transaction,
        ),
        (
            "a write returning more rows than the limit still happens",
            capped_write,
        ),
        ("values round-trip through literals", round_trip),
        ("affected rows are counted", affected_rows),
        ("the row limit stops a query and says so", row_limit),
        ("results stream: a preview, then counts", streaming),
        ("errors are QueryErrors with a message", errors),
        ("awkward identifiers are quoted", identifiers),
        ("a rolled-back transaction leaves nothing", transactions),
        ("describe finds columns, keys and indexes", describe),
        ("introspect lists tables with their columns", introspect),
        ("cancel stops a running statement", cancel),
        (
            "a refused statement doesn't count as a lost connection",
            alive_after_errors,
        ),
    ];
    let mut failed = Vec::new();
    for (name, check) in checks {
        let started = Instant::now();
        let outcome = check(conn, dialect);
        cleanup(conn);
        match outcome {
            Ok(()) => eprintln!("  ok    {name} ({:?})", started.elapsed()),
            Err(err) => {
                eprintln!("  FAIL  {name}: {err:#}");
                failed.push(format!("{name}: {err:#}"));
            }
        }
    }
    assert!(
        failed.is_empty(),
        "{} conformance check(s) failed on {}:\n  {}",
        failed.len(),
        conn.server_version(),
        failed.join("\n  ")
    );
}

const TABLES: [&str; 4] = [
    "xenon_conf_child",
    "xenon_conf_parent",
    "xenon_conf_values",
    "Xenon Conf Odd",
];

fn cleanup(conn: &dyn Connection) {
    for table in TABLES {
        let _ = conn.execute(
            &format!("drop table if exists {}", conn.quote_ident(table)),
            10,
        );
    }
}

fn rows(conn: &dyn Connection, sql: &str, limit: usize) -> Result<ResultSet> {
    match conn
        .execute(sql, limit)
        .with_context(|| sql.to_string())?
        .outcome
    {
        QueryOutcome::Rows(rows) => Ok(rows),
        QueryOutcome::Affected(n) => bail!("expected rows from `{sql}`, got {n} affected"),
    }
}

fn exec(conn: &dyn Connection, sql: &str) -> Result<QueryOutcome> {
    Ok(conn
        .execute(sql, 10)
        .with_context(|| sql.to_string())?
        .outcome)
}

/// A value as text, so engines that hand back the same data in different
/// shapes (a decimal as text or a float, a blob as bytes or `\x…`) compare
/// equal, and nothing else does: numbers compare exactly, as their shortest
/// decimal form (`12.50` and `12.5` match; `12.5` and `12.500001` don't).
fn normal(value: &Value) -> String {
    let decimal = |text: &str| {
        let text = if text.contains('.') {
            text.trim_end_matches('0').trim_end_matches('.')
        } else {
            text
        };
        if text == "-0" {
            "0".into()
        } else {
            text.to_string()
        }
    };
    match value {
        Value::Null => "NULL".into(),
        Value::Integer(n) => n.to_string(),
        Value::Real(n) => decimal(&n.to_string()),
        Value::Decimal(text) => decimal(text),
        Value::Text(text) => text.clone(),
        Value::Blob(bytes) => format!("\\x{}", crate::hex(bytes)),
    }
}

fn round_trip(conn: &dyn Connection, d: &Dialect) -> Result<()> {
    let table = "xenon_conf_values";
    exec(
        conn,
        &format!(
            "create table {table} (id {} primary key, name {}, amount {}, ratio {}, data {})",
            d.integer, d.text, d.decimal, d.real, d.blob
        ),
    )?;
    let text = |t: &str| Value::Text(t.into());
    let cases: Vec<[Value; 4]> = vec![
        [
            text("plain"),
            Value::Decimal("12.50".into()),
            Value::Real(1.5),
            Value::Blob(vec![0, 1, 255]),
        ],
        [
            text("it's \"quoted\" \\ back\\slash"),
            Value::Decimal("-0.01".into()),
            Value::Real(-2.25),
            Value::Blob(vec![]),
        ],
        [
            text("Zoë ✓ 日本 🦀\nsecond line\ttab"),
            Value::Integer(7),
            Value::Integer(0),
            Value::Null,
        ],
        [Value::Null, Value::Null, Value::Null, Value::Null],
    ];
    for (id, case) in cases.iter().enumerate() {
        let values: Vec<String> = case.iter().map(|v| conn.literal(v)).collect();
        exec(
            conn,
            &format!(
                "insert into {table} (id, name, amount, ratio, data) values ({id}, {})",
                values.join(", ")
            ),
        )?;
    }
    let read = rows(
        conn,
        &format!("select name, amount, ratio, data from {table} order by id"),
        100,
    )?;
    ensure!(
        read.rows.len() == cases.len(),
        "read back {} rows",
        read.rows.len()
    );
    for (id, (case, row)) in cases.iter().zip(&read.rows).enumerate() {
        for (column, (wrote, got)) in case.iter().zip(row).enumerate() {
            // An empty blob may come back as NULL on engines that can't tell them apart.
            if matches!(wrote, Value::Blob(b) if b.is_empty()) && got.is_null() {
                continue;
            }
            ensure!(
                normal(wrote) == normal(got),
                "row {id}, column {}: wrote {wrote:?}, read {got:?}",
                read.columns[column].name
            );
        }
    }
    Ok(())
}

fn read_only_transaction(conn: &dyn Connection, d: &Dialect) -> Result<()> {
    let Some(begin) = conn.begin_read_only() else {
        return Ok(());
    };
    let table = "xenon_conf_values";
    exec(
        conn,
        &format!("create table {table} (id {} primary key)", d.integer),
    )?;
    exec(conn, begin)?;
    let read = rows(conn, &format!("select count(*) from {table}"), 10);
    let write = conn.execute(&format!("insert into {table} (id) values (1)"), 10);
    exec(conn, d.rollback)?;
    read.context("a read inside a read-only transaction")?;
    let err = write
        .err()
        .ok_or_else(|| anyhow!("a write inside a read-only transaction went through"))?;
    let message = format!("{err:#}").to_ascii_lowercase();
    ensure!(
        message.contains("read-only transaction") || message.contains("read only transaction"),
        "refused, but not recognisably: {err:#}"
    );
    let left = rows(conn, &format!("select count(*) from {table}"), 10)?;
    ensure!(
        normal(&left.rows[0][0]) == normal(&Value::Integer(0)),
        "the write stuck"
    );
    Ok(())
}

fn capped_write(conn: &dyn Connection, d: &Dialect) -> Result<()> {
    let Some(template) = d.delete_returning else {
        return Ok(());
    };
    let table = "xenon_conf_values";
    exec(
        conn,
        &format!("create table {table} (id {} primary key)", d.integer),
    )?;
    let values: Vec<String> = (1..=30).map(|n| format!("({n})")).collect();
    exec(
        conn,
        &format!("insert into {table} (id) values {}", values.join(", ")),
    )?;
    let deleted = rows(conn, &template.replace("{table}", table), 10)?;
    ensure!(
        deleted.rows.len() == 10 && deleted.truncated,
        "the delete showed {} rows",
        deleted.rows.len()
    );
    let left = rows(conn, &format!("select count(*) from {table}"), 10)?;
    ensure!(
        normal(&left.rows[0][0]) == normal(&Value::Integer(0)),
        "the delete was undone by the row limit: {:?} rows left",
        left.rows[0][0]
    );
    Ok(())
}

fn affected_rows(conn: &dyn Connection, d: &Dialect) -> Result<()> {
    let table = "xenon_conf_values";
    exec(
        conn,
        &format!(
            "create table {table} (id {} primary key, name {})",
            d.integer, d.text
        ),
    )?;
    exec(
        conn,
        &format!("insert into {table} (id, name) values (1, 'a'), (2, 'b'), (3, 'c')"),
    )?;
    // Setting a value to what it already is still counts as a match (saving edits relies on it).
    let outcome = exec(
        conn,
        &format!("update {table} set name = 'a' where id in (1, 2)"),
    )?;
    ensure!(
        matches!(outcome, QueryOutcome::Affected(2)),
        "update: {outcome:?}"
    );
    let outcome = exec(conn, &format!("delete from {table} where id = 3"))?;
    ensure!(
        matches!(outcome, QueryOutcome::Affected(1)),
        "delete: {outcome:?}"
    );
    Ok(())
}

fn row_limit(conn: &dyn Connection, d: &Dialect) -> Result<()> {
    let capped = rows(conn, &(d.series)(50), 10)?;
    ensure!(
        capped.rows.len() == 10,
        "kept {} rows, not 10",
        capped.rows.len()
    );
    ensure!(capped.truncated, "a capped result isn't marked truncated");
    let whole = rows(conn, &(d.series)(50), 100)?;
    ensure!(
        whole.rows.len() == 50 && !whole.truncated,
        "uncapped: {} rows",
        whole.rows.len()
    );
    // Stopping a capped query must not stop the one after it (a late cancel
    // would), however quickly the next one starts.
    for round in 0..20 {
        rows(conn, &(d.series)(50), 10).with_context(|| format!("capped query, round {round}"))?;
        rows(conn, &(d.series)(3), 10)
            .with_context(|| format!("query after a capped one, round {round}"))?;
    }
    let after = rows(conn, &(d.series)(3), 10)?;
    ensure!(
        after.rows.len() == 3,
        "after a capped query: {} rows",
        after.rows.len()
    );
    Ok(())
}

fn streaming(conn: &dyn Connection, d: &Dialect) -> Result<()> {
    let mut previews = Vec::new();
    let mut counts = Vec::new();
    let result = conn.execute_streaming(
        &(d.series)(5_000),
        100_000,
        &mut |progress| match progress {
            Progress::Preview(rows) => previews.push(rows.rows.len()),
            Progress::Rows(n) => counts.push(n),
        },
    )?;
    ensure!(previews == [PREVIEW_ROWS], "previews: {previews:?}");
    ensure!(counts.contains(&4_000), "progress counts: {counts:?}");
    let QueryOutcome::Rows(rows) = result.outcome else {
        bail!("no rows");
    };
    ensure!(rows.rows.len() == 5_000, "{} rows", rows.rows.len());
    ensure!(
        normal(&rows.rows[4_999][0]) == normal(&Value::Integer(5_000)),
        "last row {:?}",
        rows.rows[4_999]
    );
    Ok(())
}

fn errors(conn: &dyn Connection, _: &Dialect) -> Result<()> {
    let err = conn
        .execute("select * from xenon_conf_missing", 10)
        .err()
        .ok_or_else(|| anyhow!("a missing table didn't fail"))?;
    let query = err
        .downcast_ref::<QueryError>()
        .ok_or_else(|| anyhow!("not a QueryError: {err:#}"))?;
    ensure!(
        query.message.contains("xenon_conf_missing"),
        "message: {}",
        query.message
    );
    // And the connection carries on.
    rows(conn, "select 1", 10)?;
    Ok(())
}

fn identifiers(conn: &dyn Connection, d: &Dialect) -> Result<()> {
    ensure!(
        conn.ident("plain_name") == "plain_name",
        "plain: {}",
        conn.ident("plain_name")
    );
    for awkward in ["Mixed", "two words", "select", "has\"quote"] {
        ensure!(conn.ident(awkward) != awkward, "{awkward} wasn't quoted");
    }
    let table = conn.quote_ident("Xenon Conf Odd");
    let (select, quote) = (conn.quote_ident("select"), conn.quote_ident("it's \"odd\""));
    exec(
        conn,
        &format!(
            "create table {table} ({select} {}, {quote} {})",
            d.integer, d.text
        ),
    )?;
    exec(
        conn,
        &format!("insert into {table} ({select}, {quote}) values (1, 'x')"),
    )?;
    let read = rows(conn, &format!("select {select}, {quote} from {table}"), 10)?;
    ensure!(
        read.columns[0].name == "select",
        "column names: {:?}",
        read.columns
    );
    ensure!(
        read.columns[1].name == "it's \"odd\"",
        "column names: {:?}",
        read.columns
    );
    Ok(())
}

fn transactions(conn: &dyn Connection, d: &Dialect) -> Result<()> {
    let table = "xenon_conf_values";
    exec(
        conn,
        &format!("create table {table} (id {} primary key)", d.integer),
    )?;
    let before = conn.in_transaction();
    exec(conn, conn.begin())?;
    exec(conn, &format!("insert into {table} (id) values (1)"))?;
    let during = conn.in_transaction();
    exec(conn, d.rollback)?;
    let after = conn.in_transaction();
    // Engines that can tell must tell it right.
    if let (Some(before), Some(during), Some(after)) = (before, during, after) {
        ensure!(
            !before && during && !after,
            "in_transaction: before {before}, during {during}, after {after}"
        );
    }
    let count = rows(conn, &format!("select count(*) from {table}"), 10)?;
    ensure!(
        normal(&count.rows[0][0]) == normal(&Value::Integer(0)),
        "count after rollback: {:?}",
        count.rows
    );
    Ok(())
}

fn describe(conn: &dyn Connection, d: &Dialect) -> Result<()> {
    let schema = conn
        .default_schema()
        .ok_or_else(|| anyhow!("no default schema"))?;
    exec(
        conn,
        &format!(
            "create table xenon_conf_parent (id {int} primary key, code {text} not null)",
            int = d.integer,
            text = d.text,
        ),
    )?;
    let cascade = if d.referential_actions {
        " on delete cascade"
    } else {
        ""
    };
    exec(
        conn,
        &format!(
            // A table constraint, not an inline `references`: MySQL ignores those.
            "create table xenon_conf_child (id {int} primary key, parent_id {int} not null, \
             note {text}, foreign key (parent_id) references xenon_conf_parent (id){cascade})",
            int = d.integer,
            text = d.text,
        ),
    )?;
    let child = conn.describe(&schema, "xenon_conf_child")?;
    let names: Vec<&str> = child.columns.iter().map(|c| c.name.as_str()).collect();
    ensure!(names == ["id", "parent_id", "note"], "columns: {names:?}");
    ensure!(child.columns[0].primary_key, "id isn't marked primary");
    ensure!(
        !child.columns[1].nullable && child.columns[2].nullable,
        "nullability: {:?}",
        child.columns
    );
    let key = child
        .foreign_keys
        .first()
        .ok_or_else(|| anyhow!("no foreign key on the child"))?;
    ensure!(key.single() == Some(("parent_id", "id")), "key: {key:?}");
    ensure!(
        key.ref_table == "xenon_conf_parent",
        "key points at {}",
        key.ref_table
    );
    if d.referential_actions {
        ensure!(
            key.on_delete.as_deref() == Some("cascade"),
            "on delete: {:?}",
            key.on_delete
        );
    }
    ensure!(
        child
            .indexes
            .iter()
            .any(|i| i.primary && i.columns == ["id"]),
        "indexes: {:?}",
        child.indexes
    );
    let parent = conn.describe(&schema, "xenon_conf_parent")?;
    ensure!(
        parent
            .referenced_by
            .iter()
            .any(|k| k.table == "xenon_conf_child"),
        "referenced by: {:?}",
        parent.referenced_by
    );
    Ok(())
}

fn introspect(conn: &dyn Connection, d: &Dialect) -> Result<()> {
    exec(
        conn,
        &format!(
            "create table xenon_conf_values (id {} primary key, name {})",
            d.integer, d.text
        ),
    )?;
    let schema = conn
        .default_schema()
        .ok_or_else(|| anyhow!("no default schema"))?;
    let schemas = conn.introspect()?;
    let table = schemas
        .iter()
        .find(|s| s.name == schema)
        .and_then(|s| s.objects.iter().find(|o| o.name == "xenon_conf_values"))
        .ok_or_else(|| anyhow!("xenon_conf_values isn't listed in {schema}"))?;
    let columns: Vec<&str> = table.columns.iter().map(|c| c.name.as_str()).collect();
    ensure!(columns == ["id", "name"], "columns: {columns:?}");
    ensure!(table.columns[0].primary_key, "id isn't marked primary");
    Ok(())
}

/// Errors the server answers with, even inside a transaction that failed,
/// leave the connection alive: only a broken one should be reconnected.
fn alive_after_errors(conn: &dyn Connection, d: &Dialect) -> Result<()> {
    ensure!(conn.alive(), "a fresh connection isn't alive");
    ensure!(
        conn.execute("select * from xenon_no_such_table", 1)
            .is_err(),
        "selecting from a missing table worked"
    );
    ensure!(conn.alive(), "not alive after a refused statement");
    conn.execute(conn.begin(), 1).context("begin")?;
    let _ = conn.execute("select * from xenon_no_such_table", 1);
    let alive = conn.alive();
    conn.execute(d.rollback, 1).context("rollback")?;
    ensure!(
        alive,
        "not alive inside a transaction a statement failed in"
    );
    Ok(())
}

fn cancel(conn: &dyn Connection, d: &Dialect) -> Result<()> {
    let handle = conn.cancel_handle();
    let (tx, rx) = mpsc::channel::<()>();
    let canceller = std::thread::spawn(move || {
        // Cancel a moment after the statement has started.
        if rx.recv_timeout(Duration::from_millis(500)).is_err() {
            handle.cancel();
        }
    });
    let started = Instant::now();
    let outcome = conn.execute(d.slow, 10);
    let elapsed = started.elapsed();
    drop(tx);
    canceller
        .join()
        .map_err(|_| anyhow!("canceller panicked"))?;
    ensure!(
        elapsed < Duration::from_secs(4),
        "took {elapsed:?}: cancel didn't stop it"
    );
    match outcome {
        Err(_) => {}
        Ok(result) if result.cancelled => {}
        Ok(result) => bail!("finished as if not cancelled: {:?}", result.outcome),
    }
    // The connection is usable afterwards.
    rows(conn, "select 1", 10).context("after cancel")?;
    Ok(())
}
