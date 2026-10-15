//! Engine-agnostic types shared by every Xenon driver.
//!
//! Drivers are blocking: the UI calls them from a background thread, and each
//! driver owns whatever runtime it needs internally. Cancellation goes through
//! a separate [`CancelHandle`] so it can be triggered while a query holds the
//! connection.

pub mod conformance;
pub mod export;
pub mod sql;

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

pub use anyhow::{Error, Result};

/// A failed statement, with where in the statement the server says it failed.
#[derive(Debug)]
pub struct QueryError {
    pub message: String,
    /// 1-based character offset into the statement, when the engine reports one.
    pub position: Option<usize>,
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for QueryError {}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    /// An exact number kept as text so no precision is lost (Postgres `numeric`, `money`).
    Decimal(String),
    Text(String),
    Blob(Vec<u8>),
}

impl Value {
    /// Text shown in a grid cell. Long values are shortened; the inspector shows them whole.
    pub fn display(&self) -> Cow<'_, str> {
        match self {
            Value::Null => Cow::Borrowed("NULL"),
            Value::Integer(v) => Cow::Owned(v.to_string()),
            Value::Real(v) => Cow::Owned(v.to_string()),
            Value::Decimal(v) | Value::Text(v) => Cow::Borrowed(v.as_str()),
            Value::Blob(v) => Cow::Owned(format!("<blob {} bytes>", v.len())),
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn is_numeric(&self) -> bool {
        matches!(self, Value::Integer(_) | Value::Real(_) | Value::Decimal(_))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    pub name: String,
    /// The declared type, when the engine reports one (e.g. `INTEGER`, `text`).
    pub type_name: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ResultSet {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Value>>,
    /// True when the query produced more rows than the requested limit.
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub enum QueryOutcome {
    Rows(ResultSet),
    /// A statement that returns no rows; carries the number of rows changed.
    Affected(u64),
}

#[derive(Clone, Debug)]
pub struct QueryResult {
    /// Outcome of the last statement executed.
    pub outcome: QueryOutcome,
    /// How many statements ran.
    pub statements: usize,
    pub elapsed: Duration,
    /// Stopped by the user (cancel) part-way through reading rows; the rows
    /// read so far are kept.
    pub cancelled: bool,
}

/// What a running query reports before it finishes.
#[derive(Debug)]
pub enum Progress {
    /// The first rows of the result being read, to show while the rest arrive.
    Preview(ResultSet),
    /// Rows read so far from the result being read.
    Rows(usize),
}

/// Rows in the early preview of a result.
pub const PREVIEW_ROWS: usize = 200;
/// How often (in rows) a running query reports its count.
pub const PROGRESS_EVERY: usize = 2_000;

/// Reports progress as a driver reads rows: call [`Feed::read`] after each row.
pub struct Feed<'a> {
    progress: &'a mut dyn FnMut(Progress),
    previewed: bool,
}

impl<'a> Feed<'a> {
    pub fn new(progress: &'a mut dyn FnMut(Progress)) -> Self {
        Self {
            progress,
            previewed: false,
        }
    }

    /// After a row lands in `rows`: hands over a preview once it has
    /// [`PREVIEW_ROWS`] rows (through `shape`, e.g. to apply column types), and
    /// a count every [`PROGRESS_EVERY`].
    pub fn read(&mut self, rows: &ResultSet, shape: impl FnOnce(&mut ResultSet)) {
        let n = rows.rows.len();
        if !self.previewed && n == PREVIEW_ROWS {
            self.previewed = true;
            let mut preview = ResultSet {
                columns: rows.columns.clone(),
                rows: rows.rows.clone(),
                truncated: false,
            };
            shape(&mut preview);
            (self.progress)(Progress::Preview(preview));
        }
        if n.is_multiple_of(PROGRESS_EVERY) {
            (self.progress)(Progress::Rows(n));
        }
    }

    /// A new result set started (in a multi-statement script): preview it too.
    pub fn next_set(&mut self) {
        self.previewed = false;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    Table,
    View,
}

#[derive(Clone, Debug)]
pub struct SchemaObject {
    pub name: String,
    pub kind: ObjectKind,
    pub columns: Vec<ObjectColumn>,
}

#[derive(Clone, Debug)]
pub struct ObjectColumn {
    pub name: String,
    pub type_name: String,
    pub primary_key: bool,
    pub not_null: bool,
}

#[derive(Clone, Debug)]
pub struct Schema {
    pub name: String,
    pub objects: Vec<SchemaObject>,
}

/// Everything about one table or view, for its Structure view and for
/// following foreign keys.
#[derive(Clone, Debug, Default)]
pub struct TableInfo {
    pub columns: Vec<ColumnInfo>,
    pub indexes: Vec<IndexInfo>,
    /// Keys in this table that point at other rows.
    pub foreign_keys: Vec<ForeignKey>,
    /// Keys in other tables (or this one) that point at this table's rows.
    pub referenced_by: Vec<ForeignKey>,
    /// The `CREATE` statement, or a view's query, when the engine can give one.
    pub definition: Option<String>,
    /// Rows by the engine's statistics (cheap, approximate), when it keeps them.
    pub estimated_rows: Option<u64>,
}

impl TableInfo {
    /// Whether some index leads with `column`, so sorting or filtering by it
    /// needn't read the whole table.
    pub fn indexed(&self, column: &str) -> bool {
        self.indexes
            .iter()
            .any(|index| index.columns.first().is_some_and(|c| c == column))
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ColumnInfo {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    /// The default expression as the engine prints it, or how the value is
    /// generated (`identity`, `auto_increment`).
    pub default: Option<String>,
    pub primary_key: bool,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct IndexInfo {
    pub name: String,
    /// Column names, or expressions for expression indexes.
    pub columns: Vec<String>,
    pub unique: bool,
    pub primary: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ForeignKey {
    pub name: String,
    /// The table holding the key.
    pub schema: String,
    pub table: String,
    pub columns: Vec<String>,
    /// The table it points at.
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    /// Referential actions in words (`cascade`, `set null`); `None` for the
    /// default, no action.
    pub on_delete: Option<String>,
    pub on_update: Option<String>,
}

impl ForeignKey {
    /// The referenced column when the key is a single column, which is what a
    /// cell can link through.
    pub fn single(&self) -> Option<(&str, &str)> {
        match (self.columns.as_slice(), self.ref_columns.as_slice()) {
            ([from], [to]) => Some((from, to)),
            _ => None,
        }
    }
}

/// A referential action in words, leaving out the default.
pub fn referential_action(action: &str) -> Option<String> {
    let words = action.trim().to_lowercase().replace('_', " ");
    (!words.is_empty() && words != "no action").then_some(words)
}

/// Bytes as lowercase hex, for blob literals.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// How a server connection uses TLS. The names follow libpq's `sslmode`; other
/// engines map them onto their own settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SslMode {
    Disable,
    /// Use TLS when the server offers it; don't verify the certificate.
    #[default]
    Prefer,
    /// Always use TLS; don't verify the certificate.
    Require,
    /// Always use TLS; verify the certificate chain but not the host name.
    VerifyCa,
    /// Always use TLS; verify the certificate chain and the host name.
    VerifyFull,
}

impl SslMode {
    pub const ALL: [SslMode; 5] = [
        SslMode::Disable,
        SslMode::Prefer,
        SslMode::Require,
        SslMode::VerifyCa,
        SslMode::VerifyFull,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SslMode::Disable => "disable",
            SslMode::Prefer => "prefer",
            SslMode::Require => "require",
            SslMode::VerifyCa => "verify-ca",
            SslMode::VerifyFull => "verify-full",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        SslMode::ALL.into_iter().find(|mode| mode.as_str() == value)
    }
}

/// Settings for a server connection, as entered in a form or parsed from a URL.
/// Empty fields mean the engine's default (e.g. `localhost`, its usual port).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConnectOptions {
    pub host: String,
    pub port: Option<u16>,
    pub database: String,
    pub user: String,
    pub password: String,
    pub ssl: SslMode,
    /// Have the server refuse writes.
    pub read_only: bool,
    /// Reach the server through an SSH tunnel; `host` and `port` are then as
    /// seen from the SSH server.
    pub ssh: Option<SshOptions>,
    /// Open the TCP connection here instead (an SSH tunnel's local end) while
    /// still using `host` to verify the server's TLS certificate.
    pub address: Option<std::net::SocketAddr>,
    /// Give up waiting for a lock after this long, so a statement queued behind
    /// another session's lock fails instead of hanging (and holding up others).
    pub lock_timeout: Option<Duration>,
}

/// An SSH server to tunnel through. Anything left empty comes from
/// `~/.ssh/config` (so `host` can be an alias there) or ssh's own defaults;
/// keys come from the agent or the config unless `key_file` is set.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SshOptions {
    pub host: String,
    pub port: Option<u16>,
    pub user: String,
    pub key_file: String,
    /// The SSH password, or the key's passphrase. Kept in the Keychain.
    pub password: String,
}

pub trait CancelHandle: Send + Sync {
    /// Ask the running query to stop. Harmless when nothing is running.
    fn cancel(&self);
}

pub trait Connection: Send + Sync {
    /// Short label for the connection, e.g. a file name or database name.
    fn name(&self) -> &str;

    /// Stable identity for local state such as query history, e.g. a file path
    /// or `user@host:port/database`. Never contains a password.
    fn id(&self) -> String {
        self.name().to_string()
    }

    /// Engine and version, e.g. `sqlite 3.50.4`.
    fn server_version(&self) -> String;

    /// Run one or more statements and return the outcome of the last.
    /// At most `row_limit` rows are kept from a result set.
    fn execute(&self, sql: &str, row_limit: usize) -> Result<QueryResult> {
        self.execute_streaming(sql, row_limit, &mut |_| {})
    }

    /// [`Connection::execute`], reporting rows as they arrive so the first
    /// can be shown at once. A cancel part-way through keeps the rows read.
    fn execute_streaming(
        &self,
        sql: &str,
        row_limit: usize,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<QueryResult>;

    fn introspect(&self) -> Result<Vec<Schema>>;

    /// Columns, indexes, keys and definition of one table or view.
    fn describe(&self, schema: &str, table: &str) -> Result<TableInfo>;

    fn cancel_handle(&self) -> Arc<dyn CancelHandle>;

    /// True for a file on this machine or a server on localhost.
    fn is_local(&self) -> bool {
        false
    }

    /// Wrap a statement so it returns its query plan as rows.
    fn explain(&self, sql: &str) -> String {
        format!("explain {sql}")
    }

    /// Quote an identifier for this engine's SQL dialect.
    fn quote_ident(&self, ident: &str) -> String {
        format!("\"{}\"", ident.replace('"', "\"\""))
    }

    /// An identifier as a person would write it: bare when that reads back as
    /// the same name (`orders`), quoted otherwise (`"Orders"`, `"order"`, `"two words"`).
    fn ident(&self, ident: &str) -> String {
        if sql::is_plain_ident(ident) {
            ident.to_string()
        } else {
            self.quote_ident(ident)
        }
    }

    /// A value written as a SQL literal, for statements Xenon generates (saving
    /// edits). Text is quoted; numbers aren't; NULL is NULL.
    fn literal(&self, value: &Value) -> String {
        match value {
            Value::Null => "NULL".into(),
            Value::Integer(n) => n.to_string(),
            Value::Real(n) if n.is_finite() => n.to_string(),
            Value::Real(n) => format!("'{n}'"),
            Value::Decimal(n) => n.clone(),
            Value::Text(text) => format!("'{}'", text.replace('\'', "''")),
            Value::Blob(bytes) => format!("X'{}'", hex(bytes)),
        }
    }

    /// Whether a transaction the user began is still open (so its locks are
    /// held until commit or rollback). `None` when the engine can't say.
    fn in_transaction(&self) -> Option<bool> {
        None
    }

    /// The statement that starts a transaction the server keeps read-only,
    /// refusing any write inside it. `None` for engines without one.
    fn begin_read_only(&self) -> Option<&'static str> {
        None
    }

    /// The statement that starts a transaction.
    fn begin(&self) -> &'static str {
        "begin"
    }

    /// True for engines that limit rows with `select top (n)` (SQL Server)
    /// rather than `limit n`.
    fn uses_top(&self) -> bool {
        false
    }

    /// The SSH server this connection tunnels through, if any.
    fn tunnel(&self) -> Option<String> {
        None
    }

    /// The schema that unqualified names resolve to (Postgres's `current_schema()`,
    /// MySQL's current database, SQLite's `main`), so generated SQL can leave it out.
    fn default_schema(&self) -> Option<String> {
        None
    }
}
