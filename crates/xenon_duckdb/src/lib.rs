//! DuckDB driver: `.duckdb` files, and CSV, TSV, Parquet and JSON files opened
//! as a view in an in-memory database, so they can be queried with SQL at once.
//!
//! Links DuckDB's prebuilt library (`DUCKDB_DOWNLOAD_LIB=1` in `.cargo/config.toml`)
//! rather than compiling it; the app bundle carries `libduckdb.dylib`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use anyhow::{Context as _, Result, anyhow};
use duckdb::arrow::datatypes::DataType;
use duckdb::types::{TimeUnit, Value as DuckValue, ValueRef};
use duckdb::{AccessMode, Config};
use xenon_driver::sql::split_statements;
use xenon_driver::{
    CancelHandle, Column, ColumnInfo, Connection, Feed, ForeignKey, IndexInfo, ObjectColumn,
    ObjectKind, Progress, QueryError, QueryOutcome, QueryResult, ResultSet, Schema, SchemaObject,
    TableInfo, Value,
};

/// What a file holds, by its first bytes or, failing that, its extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Database,
    Csv,
    Parquet,
    Json,
}

impl FileKind {
    /// A file DuckDB should open: a DuckDB database (whatever its extension) or a
    /// data file it reads directly. `None` for anything else, SQLite included.
    pub fn of(path: &Path) -> Option<Self> {
        let mut head = [0u8; 16];
        let read = std::fs::File::open(path)
            .and_then(|mut file| std::io::Read::read(&mut file, &mut head))
            .unwrap_or(0);
        if read >= 12 && &head[8..12] == b"DUCK" {
            return Some(FileKind::Database);
        }
        if read >= 4 && &head[..4] == b"PAR1" {
            return Some(FileKind::Parquet);
        }
        if head.starts_with(b"SQLite format 3") {
            return None;
        }
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "duckdb" | "ddb" => Some(FileKind::Database),
            "csv" | "tsv" | "txt" => Some(FileKind::Csv),
            "parquet" => Some(FileKind::Parquet),
            "json" | "jsonl" | "ndjson" => Some(FileKind::Json),
            _ => None,
        }
    }
}

/// The stack a query runs on: room for values nested thousands deep (they're
/// copied and printed recursively). Reserved, not used, until needed.
const QUERY_STACK: usize = 256 * 1024 * 1024;

pub struct DuckDbConnection {
    conn: Mutex<duckdb::Connection>,
    /// Refuse writes: a read-only database file is protected by DuckDB too, but
    /// a CSV opened read-only, or `copy … to` a file, isn't.
    read_only: bool,
    name: String,
    path: PathBuf,
    version: String,
    /// The database it opened on (`current_database()`), named in generated SQL.
    catalog: Option<String>,
    cancel: Arc<Interrupt>,
}

struct Interrupt {
    handle: Arc<duckdb::InterruptHandle>,
    /// Set by a user's cancel (not by the row limit), so the rows read so far are kept.
    user: AtomicBool,
}

impl CancelHandle for Interrupt {
    fn cancel(&self) {
        self.user.store(true, Ordering::SeqCst);
        self.handle.interrupt();
    }
}

impl DuckDbConnection {
    /// Open a DuckDB database, or a CSV / TSV / Parquet / JSON file as a view
    /// named after it in a fresh in-memory database.
    pub fn open(path: &Path, read_only: bool) -> Result<Self> {
        let kind = FileKind::of(path)
            .ok_or_else(|| anyhow!("{} isn't a file DuckDB opens", path.display()))?;
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let conn = match kind {
            FileKind::Database => {
                let mode = if read_only {
                    AccessMode::ReadOnly
                } else {
                    AccessMode::Automatic
                };
                duckdb::Connection::open_with_flags(path, offline()?.access_mode(mode)?)
                    .with_context(|| format!("couldn't open {}", path.display()))?
            }
            data => {
                let conn = duckdb::Connection::open_in_memory_with_flags(offline()?)?;
                let reader = match data {
                    FileKind::Parquet => "read_parquet",
                    FileKind::Json => "read_json_auto",
                    _ => "read_csv_auto",
                };
                let stem = path
                    .file_stem()
                    .map_or("data".into(), |s| s.to_string_lossy().into_owned());
                let source = path.display().to_string().replace('\'', "''");
                conn.execute_batch(&format!(
                    "create view \"{}\" as select * from {reader}('{source}')",
                    stem.replace('"', "\"\"")
                ))
                .map_err(query_error)
                .with_context(|| format!("couldn't read {}", path.display()))?;
                conn
            }
        };
        Self::wrap(conn, name, path.to_path_buf(), read_only)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::wrap(
            duckdb::Connection::open_in_memory_with_flags(offline()?)?,
            "memory".into(),
            PathBuf::from(":memory:"),
            false,
        )
    }

    fn wrap(
        conn: duckdb::Connection,
        name: String,
        path: PathBuf,
        read_only: bool,
    ) -> Result<Self> {
        let version: String = conn
            .query_row("select version()", [], |row| row.get(0))
            .unwrap_or_default();
        let catalog: Option<String> = conn
            .query_row("select current_database()", [], |row| row.get(0))
            .ok();
        // Times with a zone are shown in UTC: read typed ones in UTC too, or a
        // value typed as shown is stored hours off (and conflict checks on it
        // never match).
        let _ = conn.execute_batch("set TimeZone = 'UTC'");
        let cancel = Arc::new(Interrupt {
            handle: conn.interrupt_handle(),
            user: AtomicBool::new(false),
        });
        Ok(Self {
            conn: Mutex::new(conn),
            read_only,
            name,
            path,
            version: format!("duckdb {}", version.trim_start_matches('v')),
            catalog,
            cancel,
        })
    }

    fn run_streaming(
        &self,
        sql: &str,
        row_limit: usize,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<QueryResult> {
        if self.read_only
            && let Some((_, what)) = xenon_driver::sql::assess(sql)
        {
            return Err(QueryError {
                message: format!("This is a read-only connection, so {what} isn't allowed."),
                position: None,
            }
            .into());
        }
        self.run(sql, row_limit, progress)
    }

    fn lock(&self) -> MutexGuard<'_, duckdb::Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn run(
        &self,
        sql: &str,
        row_limit: usize,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<QueryResult> {
        let conn = self.lock();
        self.cancel.user.store(false, Ordering::SeqCst);
        let started = Instant::now();
        let mut feed = Feed::new(progress);
        let writes = xenon_driver::sql::writes(sql);
        let mut outcome = None;
        let mut earlier = Vec::new();
        let mut statements = 0;
        let mut cancelled = false;

        for range in split_statements(sql) {
            // DuckDB drops an interrupt that finds nothing running: one that came
            // between statements stops the rest here.
            if self.cancel.user.load(Ordering::SeqCst) {
                return Err(anyhow!("Cancelled before the end of the script."));
            }
            let text = &sql[range];
            statements += 1;
            let mut stmt = conn.prepare(text).map_err(query_error)?;
            let mut rows = stmt.query([]).map_err(query_error)?;
            let (columns, width) = {
                let stmt = rows.as_ref().expect("rows from a statement");
                let width = stmt.column_count();
                let columns: Vec<Column> = stmt
                    .column_names()
                    .into_iter()
                    .enumerate()
                    .map(|(ix, name)| Column {
                        name,
                        type_name: Some(type_name(&stmt.column_type(ix))),
                    })
                    .collect();
                (columns, width)
            };
            // Writes answer with one `Count` column; DDL with none.
            let verb = text
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if width == 0 {
                earlier.extend(outcome.replace(QueryOutcome::Affected(0)));
                continue;
            }
            if width == 1
                && columns[0].name == "Count"
                && matches!(verb.as_str(), "insert" | "update" | "delete" | "merge")
            {
                let count = match rows.next().map_err(query_error)? {
                    Some(row) => match row.get_ref(0)? {
                        ValueRef::BigInt(n) => n.max(0) as u64,
                        other => to_value(other).display().parse().unwrap_or(0),
                    },
                    None => 0,
                };
                earlier.extend(outcome.replace(QueryOutcome::Affected(count)));
                continue;
            }

            let mut result = ResultSet {
                columns,
                ..Default::default()
            };
            feed.next_set();
            loop {
                let row = match rows.next() {
                    Ok(Some(row)) => row,
                    Ok(None) => break,
                    // Interrupted (cancel) while reading: keep the rows read.
                    Err(_)
                        if self.cancel.user.load(Ordering::SeqCst) && !result.rows.is_empty() =>
                    {
                        cancelled = true;
                        break;
                    }
                    Err(err) => return Err(query_error(err)),
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
            earlier.extend(outcome.replace(QueryOutcome::Rows(result)));
            if cancelled {
                break;
            }
        }
        Ok(QueryResult {
            outcome: outcome.ok_or_else(|| anyhow!("no statement to run"))?,
            earlier,
            statements,
            elapsed: started.elapsed(),
            cancelled,
        })
    }
}

impl Connection for DuckDbConnection {
    fn name(&self) -> &str {
        &self.name
    }

    fn id(&self) -> String {
        self.path.display().to_string()
    }

    fn server_version(&self) -> String {
        self.version.clone()
    }

    fn execute_streaming(
        &self,
        sql: &str,
        row_limit: usize,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<QueryResult> {
        // Committing a transaction an error already aborted keeps nothing:
        // say so, rather than "committed".
        if !xenon_driver::sql::several_statements(sql)
            && xenon_driver::sql::transaction_effect(sql) == Some(false)
            && !sql.to_ascii_lowercase().contains("rollback")
            && !sql.to_ascii_lowercase().contains("abort")
        {
            let conn = self.lock();
            if let Err(err) = conn.execute_batch("select 1")
                && err.to_string().to_ascii_lowercase().contains("aborted")
            {
                let _ = conn.execute_batch("rollback");
                return Err(anyhow!(
                    "The transaction had already failed, so nothing was committed: it was rolled back."
                ));
            }
        }
        // A deeply nested value (a list in a list, 600 times over) is copied
        // and printed recursively, here and in the duckdb crate. The threads
        // background work runs on have small stacks, and running out aborts the
        // whole app: run the query on one with room, passing progress back.
        let (sender, receiver) = std::sync::mpsc::channel::<Progress>();
        std::thread::scope(|scope| {
            let worker = std::thread::Builder::new()
                .name("xenon-duckdb".into())
                .stack_size(QUERY_STACK)
                .spawn_scoped(scope, move || {
                    self.run_streaming(sql, row_limit, &mut |p| {
                        let _ = sender.send(p);
                    })
                })?;
            for p in receiver {
                progress(p);
            }
            worker
                .join()
                .unwrap_or_else(|_| Err(anyhow!("DuckDB stopped unexpectedly while running this.")))
        })
    }

    fn introspect(&self) -> Result<Vec<Schema>> {
        const QUERY: &str = "
            with objects as (
                select schema_name, table_name, 'table' as kind from duckdb_tables()
                where database_name = current_database()
                union all
                select schema_name, view_name, 'view' from duckdb_views()
                where database_name = current_database() and not internal
            ),
            keys as (
                select schema_name, table_name, unnest(constraint_column_names) as column_name
                from duckdb_constraints()
                where database_name = current_database() and constraint_type = 'PRIMARY KEY'
            )
            select c.schema_name, c.table_name, o.kind, c.column_name, c.data_type,
                   not c.is_nullable, k.column_name is not null
            from duckdb_columns() c
            join objects o on o.schema_name = c.schema_name and o.table_name = c.table_name
            left join keys k on k.schema_name = c.schema_name and k.table_name = c.table_name
                            and k.column_name = c.column_name
            where c.database_name = current_database()
            order by c.schema_name = 'main' desc, c.schema_name, c.table_name, c.column_index";
        let conn = self.lock();
        let mut stmt = conn.prepare(QUERY).map_err(query_error)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    ObjectColumn {
                        name: row.get(3)?,
                        type_name: row.get(4)?,
                        not_null: row.get(5)?,
                        primary_key: row.get(6)?,
                    },
                ))
            })
            .map_err(query_error)?
            .collect::<duckdb::Result<Vec<_>>>()
            .map_err(query_error)?;
        let mut schemas: Vec<Schema> = Vec::new();
        for (schema, table, kind, column) in rows {
            if schemas.last().is_none_or(|s| s.name != schema) {
                schemas.push(Schema {
                    name: schema,
                    objects: Vec::new(),
                });
            }
            let objects = &mut schemas.last_mut().expect("pushed above").objects;
            if objects.last().is_none_or(|o| o.name != table) {
                objects.push(SchemaObject {
                    name: table,
                    kind: if kind == "view" {
                        ObjectKind::View
                    } else {
                        ObjectKind::Table
                    },
                    columns: Vec::new(),
                });
            }
            objects
                .last_mut()
                .expect("pushed above")
                .columns
                .push(column);
        }
        Ok(schemas)
    }

    fn describe(&self, schema: &str, table: &str) -> Result<TableInfo> {
        let conn = self.lock();
        let mut info = TableInfo::default();

        // Constraints first: primary keys mark columns, the rest become indexes and keys.
        let mut constraints = conn
            .prepare(
                "select constraint_type, constraint_name, table_name, constraint_column_names,
                        referenced_table, referenced_column_names
                 from duckdb_constraints()
                 where database_name = current_database() and schema_name = ?
                   and (table_name = ? or referenced_table = ?)
                   and constraint_type in ('PRIMARY KEY', 'UNIQUE', 'FOREIGN KEY')
                 order by constraint_index",
            )
            .map_err(query_error)?;
        let found = constraints
            .query_map([schema, table, table], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    row.get::<_, String>(2)?,
                    strings(row.get_ref(3)?.to_owned()),
                    row.get::<_, Option<String>>(4)?,
                    strings(row.get_ref(5)?.to_owned()),
                ))
            })
            .map_err(query_error)?
            .collect::<duckdb::Result<Vec<_>>>()
            .map_err(query_error)?;
        let mut primary: Vec<String> = Vec::new();
        for (kind, name, owner, columns, referenced, ref_columns) in found {
            match kind.as_str() {
                "PRIMARY KEY" if owner == table => {
                    primary = columns.clone();
                    info.indexes.insert(
                        0,
                        IndexInfo {
                            name: if name.is_empty() {
                                "primary key".into()
                            } else {
                                name
                            },
                            columns,
                            unique: true,
                            primary: true,
                        },
                    );
                }
                "UNIQUE" if owner == table => info.indexes.push(IndexInfo {
                    name,
                    columns,
                    unique: true,
                    primary: false,
                }),
                "FOREIGN KEY" => {
                    let Some(referenced) = referenced else {
                        continue;
                    };
                    let key = ForeignKey {
                        name: if name.is_empty() {
                            format!("{owner}_{}_fkey", columns.join("_"))
                        } else {
                            name
                        },
                        schema: schema.into(),
                        table: owner.clone(),
                        columns,
                        ref_schema: schema.into(),
                        ref_table: referenced.clone(),
                        ref_columns,
                        // DuckDB has no referential actions.
                        on_delete: None,
                        on_update: None,
                    };
                    if referenced == table {
                        info.referenced_by.push(key.clone());
                    }
                    if owner == table {
                        info.foreign_keys.push(key);
                    }
                }
                _ => {}
            }
        }

        let mut columns = conn
            .prepare(
                "select column_name, data_type, is_nullable, column_default, comment
                 from duckdb_columns()
                 where database_name = current_database() and schema_name = ? and table_name = ?
                 order by column_index",
            )
            .map_err(query_error)?;
        info.columns = columns
            .query_map([schema, table], |row| {
                let name: String = row.get(0)?;
                let type_name: String = row.get(1)?;
                Ok(ColumnInfo {
                    primary_key: primary.contains(&name),
                    name,
                    options: xenon_driver::enum_options(&type_name),
                    type_name,
                    nullable: row.get(2)?,
                    default: row.get(3)?,
                    comment: row.get::<_, Option<String>>(4)?.filter(|c| !c.is_empty()),
                })
            })
            .map_err(query_error)?
            .collect::<duckdb::Result<Vec<_>>>()
            .map_err(query_error)?;

        let mut indexes = conn
            .prepare(
                "select index_name, is_unique, is_primary, expressions
                 from duckdb_indexes()
                 where database_name = current_database() and schema_name = ? and table_name = ?
                 order by index_name",
            )
            .map_err(query_error)?;
        let more = indexes
            .query_map([schema, table], |row| {
                let expressions: Option<String> = row.get(3)?;
                Ok(IndexInfo {
                    name: row.get(0)?,
                    unique: row.get(1)?,
                    primary: row.get(2)?,
                    columns: expressions
                        .as_deref()
                        .map(index_columns)
                        .unwrap_or_default(),
                })
            })
            .map_err(query_error)?
            .collect::<duckdb::Result<Vec<_>>>()
            .map_err(query_error)?;
        info.indexes.extend(more);

        info.estimated_rows = conn
            .query_row(
                "select estimated_size from duckdb_tables()
                 where database_name = current_database() and schema_name = ? and table_name = ?",
                [schema, table],
                |row| row.get::<_, Option<i64>>(0),
            )
            .ok()
            .flatten()
            .and_then(|n| u64::try_from(n).ok());
        info.definition = conn
            .query_row(
                "select sql from duckdb_tables()
                 where database_name = current_database() and schema_name = ? and table_name = ?
                 union all
                 select sql from duckdb_views()
                 where database_name = current_database() and schema_name = ? and view_name = ?",
                [schema, table, schema, table],
                |row| row.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten();
        Ok(info)
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        self.cancel.clone()
    }

    fn unchanged(&self, column: &str, type_name: &str, loaded: &Value) -> Option<String> {
        if !loaded.is_null() && type_name.eq_ignore_ascii_case("json") {
            return Some(format!("{column}::varchar = {}", self.literal(loaded)));
        }
        xenon_driver::default_unchanged(self, column, loaded)
    }

    fn is_local(&self) -> bool {
        true
    }

    fn default_schema(&self) -> Option<String> {
        Some("main".into())
    }

    fn catalog(&self) -> Option<String> {
        self.catalog.clone()
    }

    fn literal(&self, value: &Value) -> String {
        match value {
            Value::Blob(bytes) => {
                let escaped: String = bytes.iter().map(|b| format!("\\x{b:02X}")).collect();
                format!("'{escaped}'::BLOB")
            }
            Value::Real(n) if !n.is_finite() => format!("'{n}'::DOUBLE"),
            Value::Null => "NULL".into(),
            Value::Integer(n) => n.to_string(),
            Value::Real(n) => n.to_string(),
            Value::Decimal(n) => n.clone(),
            Value::Text(text) => format!("'{}'", text.replace('\'', "''")),
        }
    }
}

/// DuckDB set not to fetch anything from the internet by itself: it would
/// otherwise download and load extensions (`httpfs`, `json`…) on first use.
fn offline() -> Result<Config> {
    Ok(Config::default()
        .with("autoinstall_known_extensions", "false")?
        .with("autoload_known_extensions", "false")?)
}

/// DuckDB's message as a [`QueryError`], like the other engines' errors.
fn query_error(err: duckdb::Error) -> anyhow::Error {
    let message = match err {
        duckdb::Error::DuckDBFailure(_, Some(message)) => message,
        other => other.to_string(),
    };
    QueryError {
        message,
        position: None,
    }
    .into()
}

/// The text of a list of strings (constraint column names).
fn strings(value: DuckValue) -> Vec<String> {
    match value {
        DuckValue::List(items) | DuckValue::Array(items) => items
            .into_iter()
            .filter_map(|item| match item {
                DuckValue::Text(text) => Some(text),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `[a, lower(b)]` from `duckdb_indexes().expressions` as its parts.
fn index_columns(expressions: &str) -> Vec<String> {
    let inner = expressions
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']');
    inner
        .split(", ")
        .map(|part| part.trim().trim_matches('\'').trim_matches('"').to_string())
        .filter(|part| !part.is_empty())
        .collect()
}

/// A column's type as DuckDB would name it in SQL.
fn type_name(data_type: &DataType) -> String {
    match data_type {
        DataType::Boolean => "BOOLEAN".into(),
        DataType::Int8 => "TINYINT".into(),
        DataType::Int16 => "SMALLINT".into(),
        DataType::Int32 => "INTEGER".into(),
        DataType::Int64 => "BIGINT".into(),
        DataType::UInt8 => "UTINYINT".into(),
        DataType::UInt16 => "USMALLINT".into(),
        DataType::UInt32 => "UINTEGER".into(),
        DataType::UInt64 => "UBIGINT".into(),
        DataType::Float16 | DataType::Float32 => "FLOAT".into(),
        DataType::Float64 => "DOUBLE".into(),
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => "VARCHAR".into(),
        DataType::Binary | DataType::LargeBinary | DataType::BinaryView => "BLOB".into(),
        DataType::FixedSizeBinary(16) => "HUGEINT".into(),
        DataType::Date32 | DataType::Date64 => "DATE".into(),
        DataType::Time32(_) | DataType::Time64(_) => "TIME".into(),
        DataType::Timestamp(_, Some(_)) => "TIMESTAMPTZ".into(),
        DataType::Timestamp(_, None) => "TIMESTAMP".into(),
        DataType::Interval(_) | DataType::Duration(_) => "INTERVAL".into(),
        DataType::Decimal128(p, s) | DataType::Decimal256(p, s) => format!("DECIMAL({p},{s})"),
        DataType::List(_) | DataType::LargeList(_) | DataType::FixedSizeList(..) => "LIST".into(),
        DataType::Struct(_) => "STRUCT".into(),
        DataType::Map(..) => "MAP".into(),
        DataType::Dictionary(..) => "ENUM".into(),
        DataType::Union(..) => "UNION".into(),
        other => format!("{other:?}").to_uppercase(),
    }
}

/// A value as Xenon shows it: numbers as numbers (exact decimals and huge
/// integers as text), dates and times as DuckDB prints them, nested values as
/// DuckDB's literal syntax.
fn to_value(value: ValueRef<'_>) -> Value {
    let exact = |text: String| Value::Decimal(text);
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Boolean(b) => Value::Text(b.to_string()),
        ValueRef::TinyInt(n) => Value::Integer(n.into()),
        ValueRef::SmallInt(n) => Value::Integer(n.into()),
        ValueRef::Int(n) => Value::Integer(n.into()),
        ValueRef::BigInt(n) => Value::Integer(n),
        ValueRef::UTinyInt(n) => Value::Integer(n.into()),
        ValueRef::USmallInt(n) => Value::Integer(n.into()),
        ValueRef::UInt(n) => Value::Integer(n.into()),
        ValueRef::UBigInt(n) => {
            i64::try_from(n).map_or_else(|_| exact(n.to_string()), Value::Integer)
        }
        ValueRef::HugeInt(n) => {
            i64::try_from(n).map_or_else(|_| exact(n.to_string()), Value::Integer)
        }
        ValueRef::UHugeInt(n) => {
            i64::try_from(n).map_or_else(|_| exact(n.to_string()), Value::Integer)
        }
        // Through its shortest decimal form, so 0.1f32 shows as 0.1.
        ValueRef::Float(n) => Value::Real(n.to_string().parse().unwrap_or(n.into())),
        ValueRef::Double(n) => Value::Real(n),
        ValueRef::Decimal(d) => exact(d.to_string()),
        ValueRef::Text(bytes) => Value::Text(String::from_utf8_lossy(bytes).into_owned()),
        ValueRef::Blob(bytes) | ValueRef::Geometry(bytes) => Value::Blob(bytes.to_vec()),
        other => Value::Text(render(&other.to_owned())),
    }
}

/// Nested and temporal values as text, the way DuckDB prints them.
fn render(value: &DuckValue) -> String {
    match value {
        DuckValue::Null => "NULL".into(),
        DuckValue::Boolean(b) => b.to_string(),
        DuckValue::TinyInt(n) => n.to_string(),
        DuckValue::SmallInt(n) => n.to_string(),
        DuckValue::Int(n) => n.to_string(),
        DuckValue::BigInt(n) => n.to_string(),
        DuckValue::HugeInt(n) => n.to_string(),
        DuckValue::UHugeInt(n) => n.to_string(),
        DuckValue::UTinyInt(n) => n.to_string(),
        DuckValue::USmallInt(n) => n.to_string(),
        DuckValue::UInt(n) => n.to_string(),
        DuckValue::UBigInt(n) => n.to_string(),
        DuckValue::Float(n) => n.to_string(),
        DuckValue::Double(n) => n.to_string(),
        DuckValue::Decimal(d) => d.to_string(),
        DuckValue::Text(text) | DuckValue::Enum(text) => text.clone(),
        DuckValue::Blob(bytes) | DuckValue::Geometry(bytes) => {
            bytes.iter().map(|b| format!("\\x{b:02X}")).collect()
        }
        DuckValue::Date32(days) => date(i64::from(*days)),
        DuckValue::Time64(unit, n) => time(micros(*unit, *n)),
        DuckValue::Timestamp(unit, n) => {
            let us = micros(*unit, *n);
            let (days, rest) = (us.div_euclid(86_400_000_000), us.rem_euclid(86_400_000_000));
            format!("{} {}", date(days), time(rest))
        }
        DuckValue::Interval {
            months,
            days,
            nanos,
        } => interval(*months, *days, *nanos),
        DuckValue::List(items) | DuckValue::Array(items) => {
            let parts: Vec<String> = items.iter().map(nested).collect();
            format!("[{}]", parts.join(", "))
        }
        DuckValue::Struct(fields) => {
            let parts: Vec<String> = fields
                .iter()
                .map(|(name, value)| format!("'{name}': {}", nested(value)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        DuckValue::Map(entries) => {
            let parts: Vec<String> = entries
                .iter()
                .map(|(key, value)| format!("{}={}", render(key), render(value)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        DuckValue::Union(inner) => render(inner),
        // Types added to DuckDB after this was written.
        other => format!("{other:?}"),
    }
}

/// A value inside a list or struct: text is quoted, as DuckDB prints it.
fn nested(value: &DuckValue) -> String {
    match value {
        DuckValue::Text(text) => format!("'{}'", text.replace('\'', "''")),
        other => render(other),
    }
}

fn micros(unit: TimeUnit, n: i64) -> i64 {
    match unit {
        TimeUnit::Second => n.saturating_mul(1_000_000),
        TimeUnit::Millisecond => n.saturating_mul(1_000),
        TimeUnit::Microsecond => n,
        TimeUnit::Nanosecond => n / 1_000,
    }
}

/// `YYYY-MM-DD` for days since 1970-01-01 (Howard Hinnant's civil-from-days).
fn date(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// `HH:MM:SS`, with microseconds when there are any.
fn time(micros: i64) -> String {
    let seconds = micros / 1_000_000;
    let fraction = micros % 1_000_000;
    let clock = format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        seconds / 60 % 60,
        seconds % 60
    );
    if fraction == 0 {
        clock
    } else {
        format!("{clock}.{fraction:06}")
    }
}

fn interval(months: i32, days: i32, nanos: i64) -> String {
    let mut parts = Vec::new();
    let (years, months) = (months / 12, months % 12);
    let plural = |n: i64, unit: &str| format!("{n} {unit}{}", if n.abs() == 1 { "" } else { "s" });
    if years != 0 {
        parts.push(plural(years.into(), "year"));
    }
    if months != 0 {
        parts.push(plural(months.into(), "month"));
    }
    if days != 0 {
        parts.push(plural(days.into(), "day"));
    }
    if nanos != 0 || parts.is_empty() {
        let sign = if nanos < 0 { "-" } else { "" };
        parts.push(format!("{sign}{}", time((nanos / 1_000).abs())));
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(conn: &DuckDbConnection, sql: &str) -> ResultSet {
        match conn.execute(sql, 1_000).unwrap().outcome {
            QueryOutcome::Rows(rows) => rows,
            other => panic!("expected rows, got {other:?}"),
        }
    }

    #[test]
    fn conformance() {
        let conn = DuckDbConnection::open_in_memory().unwrap();
        xenon_driver::conformance::run(
            &conn,
            &xenon_driver::conformance::Dialect {
                integer: "integer",
                text: "varchar",
                decimal: "decimal(12,2)",
                real: "double",
                blob: "blob",
                series: |n| format!("select i from range(1, {n} + 1) t(i)"),
                slow: "select count(*) from range(10000000000) a",
                rollback: "rollback",
                referential_actions: false,
                delete_returning: Some("delete from {table} returning id"),
            },
        );
    }

    #[test]
    fn values_show_as_duckdb_prints_them() {
        let conn = DuckDbConnection::open_in_memory().unwrap();
        let read = rows(
            &conn,
            "select 12345678901234567890::hugeint, 1.10::decimal(5,2), 0.1::float, true,
                    date '2024-02-29', timestamp '1969-12-31 23:59:59.5', time '13:04:05',
                    interval 14 months + interval 3 days, [1, 2], {'a': 'x', 'b': [true]},
                    map {'k': 1}, 'b'::enum ('a', 'b'), '\\xAB'::blob",
        );
        let shown: Vec<String> = read.rows[0]
            .iter()
            .map(|v| v.display().into_owned())
            .collect();
        assert_eq!(
            shown,
            [
                "12345678901234567890",
                "1.10",
                "0.1",
                "true",
                "2024-02-29",
                "1969-12-31 23:59:59.500000",
                "13:04:05",
                "1 year 2 months 3 days",
                "[1, 2]",
                "{'a': 'x', 'b': [true]}",
                "{k=1}",
                "b",
                "<blob 1 bytes>",
            ]
        );
        assert_eq!(read.columns[1].type_name.as_deref(), Some("DECIMAL(5,2)"));
    }

    #[test]
    fn a_deeply_nested_value_doesnt_crash() {
        // GPUI runs background work on threads with 512 KB stacks.
        let depth = 800;
        let sql = format!("select {}1{}", "[".repeat(depth), "]".repeat(depth));
        let shown = std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || {
                let db = DuckDbConnection::open_in_memory().unwrap();
                db.execute("set max_expression_depth to 10000", 0).unwrap();
                match db.execute(&sql, 1).unwrap().outcome {
                    QueryOutcome::Rows(rows) => rows.rows[0][0].display().to_string(),
                    other => panic!("{other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
        assert!(
            shown.starts_with("[[[") && shown.contains('1'),
            "{}",
            &shown[..20]
        );
    }

    /// A cancel between the statements of a script stops it.
    #[test]
    fn a_cancel_stops_a_script_of_many_statements() {
        let db = DuckDbConnection::open_in_memory().unwrap();
        db.execute("create table t (n int)", 0).unwrap();
        let script: String = (0..20_000)
            .map(|n| format!("insert into t values ({n});"))
            .collect();
        let cancel = db.cancel_handle();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            cancel.cancel();
        });
        let _ = db.execute(&script, 0);
        stopper.join().unwrap();
        let count = match db.execute("select count(*) from t", 1).unwrap().outcome {
            QueryOutcome::Rows(rows) => rows.rows[0][0].clone(),
            other => panic!("{other:?}"),
        };
        let Value::Integer(count) = count else {
            panic!("{count:?}")
        };
        assert!(count < 20_000, "the cancel was lost ({count} rows)");
    }

    #[test]
    fn committing_a_failed_transaction_says_nothing_was_kept() {
        let db = DuckDbConnection::open_in_memory().unwrap();
        db.execute("create table t (n int primary key)", 0).unwrap();
        db.execute("begin", 0).unwrap();
        db.execute("insert into t values (1)", 0).unwrap();
        assert!(db.execute("insert into t values (1)", 0).is_err());
        let err = db.execute("commit", 0).unwrap_err().to_string();
        assert!(err.contains("nothing was committed"), "{err}");
        // And the session works again.
        assert!(db.execute("select count(*) from t", 1).is_ok());
    }

    #[test]
    fn a_time_with_a_zone_is_read_as_it_is_shown() {
        let db = DuckDbConnection::open_in_memory().unwrap();
        db.execute(
            "create table t (ts timestamptz); insert into t values ('2026-07-01 12:00:00')",
            0,
        )
        .unwrap();
        let shown = match db.execute("select ts from t", 1).unwrap().outcome {
            QueryOutcome::Rows(rows) => rows.rows[0][0].display().to_string(),
            other => panic!("{other:?}"),
        };
        assert!(shown.starts_with("2026-07-01 12:00:00"), "{shown}");
        // Typed back as shown, it matches (so a conflict check on it can).
        let found = match db
            .execute(&format!("select count(*) from t where ts = '{shown}'"), 1)
            .unwrap()
            .outcome
        {
            QueryOutcome::Rows(rows) => rows.rows[0][0].clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(found, Value::Integer(1));
    }

    #[test]
    fn csv_and_parquet_open_as_views() {
        let dir = std::env::temp_dir().join(format!("xenon-duckdb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let csv = dir.join("people.csv");
        std::fs::write(&csv, "id,name\n1,Ada\n2,Grace\n").unwrap();
        // Read-write: the test copies it out to Parquet below.
        let conn = DuckDbConnection::open(&csv, false).unwrap();
        let read = rows(&conn, "select name from people order by id");
        assert_eq!(read.rows[1][0], Value::Text("Grace".into()));
        let tables = conn.introspect().unwrap();
        assert_eq!(tables[0].objects[0].name, "people");
        assert_eq!(tables[0].objects[0].kind, ObjectKind::View);

        let parquet = dir.join("people.parquet");
        conn.execute(
            &format!(
                "copy (select * from people) to '{}' (format parquet)",
                parquet.display()
            ),
            10,
        )
        .unwrap();
        assert_eq!(FileKind::of(&parquet), Some(FileKind::Parquet));
        let conn = DuckDbConnection::open(&parquet, true).unwrap();
        assert_eq!(
            rows(&conn, "select count(*) from people").rows[0][0],
            Value::Integer(2)
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn database_files_are_recognised_by_their_header() {
        let dir = std::env::temp_dir().join(format!("xenon-duckdb-db-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A DuckDB database with a .db extension, as people often name them.
        let path = dir.join("analytics.db");
        {
            let conn = duckdb::Connection::open(&path).unwrap();
            conn.execute_batch("create table t (id integer)").unwrap();
        }
        assert_eq!(FileKind::of(&path), Some(FileKind::Database));
        let conn = DuckDbConnection::open(&path, true).unwrap();
        assert!(conn.execute("insert into t values (1)", 10).is_err());
        assert_eq!(FileKind::of(Path::new("/nonexistent/x.sqlite")), None);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn read_only_holds_for_files_and_never_fetches_extensions() {
        let dir = std::env::temp_dir().join(format!("xenon-duckdb-ro-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let csv = dir.join("people.csv");
        std::fs::write(&csv, "id,name\n1,Ada\n").unwrap();
        let conn = DuckDbConnection::open(&csv, true).unwrap();
        let out = dir.join("copy.csv");
        for write in [
            "create table t (id integer)".to_string(),
            format!("copy (select * from people) to '{}'", out.display()),
            "attach ':memory:' as other".to_string(),
            "install httpfs".to_string(),
        ] {
            let err = conn.execute(&write, 10).unwrap_err().to_string();
            assert!(err.contains("read-only connection"), "{write}: {err}");
        }
        assert!(!out.exists());
        assert_eq!(
            rows(&conn, "select count(*) from people").rows[0][0],
            Value::Integer(1)
        );
        // Extensions are never downloaded or loaded behind the user's back.
        let settings = rows(
            &conn,
            "select current_setting('autoinstall_known_extensions'), \
                    current_setting('autoload_known_extensions')",
        );
        assert_eq!(
            settings.rows[0],
            [Value::Text("false".into()), Value::Text("false".into())]
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn calendar_dates() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(-1), "1969-12-31");
        assert_eq!(date(19_782), "2024-02-29");
        assert_eq!(time(3_723_000_001), "01:02:03.000001");
    }
}
