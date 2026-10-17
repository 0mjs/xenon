//! The edit check's `unchanged` conditions against real DuckDB values: each
//! matches the row as loaded, and stops matching when someone changes it.

use xenon_driver::{Connection, QueryOutcome, Value};
use xenon_duckdb::DuckDbConnection;

fn count(conn: &DuckDbConnection, sql: &str) -> i64 {
    match conn.execute(sql, 10).unwrap().outcome {
        QueryOutcome::Rows(rows) => match rows.rows[0][0] {
            Value::Integer(n) => n,
            ref other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
}

#[test]
fn loaded_values_compare_back() {
    let conn = DuckDbConnection::open_in_memory().unwrap();
    conn.execute(
        r"create table t as select
            ['it''s', 'a\b', 'x', 'q\''x', ''] list,
            {'k': 'it''s', 'n': [1]} record,
            map {'a''b': 'c', 'k': 'it''s'} pairs,
            map {1: 'x'} numbered,
            [{'x': 'y''z'}] nested,
            '2024-01-01 00:00:00.123456789'::timestamp_ns ns,
            '12:00:00+02'::timetz tz,
            '0101'::bit bits,
            ('Abc' collate nocase) as name",
        10,
    )
    .unwrap();
    let QueryOutcome::Rows(rows) = conn.execute("select * from t", 10).unwrap().outcome else {
        panic!("rows");
    };
    let mut checked = Vec::new();
    for (ix, column) in rows.columns.iter().enumerate() {
        let kind = column.type_name.as_deref().unwrap_or("");
        match conn.unchanged(&column.name, kind, &rows.rows[0][ix]) {
            Some(condition) => {
                let sql = format!("select count(*) from t where {condition}");
                assert_eq!(count(&conn, &sql), 1, "{sql}");
                checked.push(column.name.as_str());
            }
            None => assert!(
                ["ns", "tz", "bits"].contains(&column.name.as_str()),
                "{}",
                column.name
            ),
        }
    }
    assert_eq!(
        checked,
        ["list", "record", "pairs", "numbered", "nested", "name"]
    );
}

#[test]
fn collations_dont_hide_a_change() {
    let conn = DuckDbConnection::open_in_memory().unwrap();
    conn.execute(
        "create table t (a varchar collate nocase, b varchar collate noaccent, c varchar);
         insert into t values ('old', 'cafe', 'x');
         set default_collation = 'nocase'",
        10,
    )
    .unwrap();
    let check = |column: &str, loaded: &str| {
        let condition = conn
            .unchanged(column, "VARCHAR", &Value::Text(loaded.into()))
            .unwrap();
        count(&conn, &format!("select count(*) from t where {condition}"))
    };
    assert_eq!(
        (check("a", "old"), check("b", "cafe"), check("c", "x")),
        (1, 1, 1)
    );
    // Someone else's change in case or accents.
    assert_eq!(
        (check("a", "OLD"), check("b", "café"), check("c", "X")),
        (0, 0, 0)
    );
}

/// After `use other`, the tables (and so the keys an edit goes by) are still
/// the ones of the database the connection opened on.
#[test]
fn keys_come_from_the_connections_own_catalog() {
    let conn = DuckDbConnection::open_in_memory().unwrap();
    conn.execute(
        "create table t (id integer primary key, code varchar);
         attach ':memory:' as other;
         create table other.main.t (id integer, code varchar primary key);
         use other",
        10,
    )
    .unwrap();
    let info = conn.describe("main", "t").unwrap();
    let keys: Vec<&str> = info
        .columns
        .iter()
        .filter(|c| c.primary_key)
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(keys, ["id"]);
    let schemas = conn.introspect().unwrap();
    let t = schemas
        .iter()
        .flat_map(|s| &s.objects)
        .find(|t| t.name == "t")
        .expect("t");
    let keys: Vec<&str> = t
        .columns
        .iter()
        .filter(|c| c.primary_key)
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(keys, ["id"]);
}
