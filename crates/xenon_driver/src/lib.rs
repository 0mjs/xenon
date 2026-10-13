//! Engine-agnostic types shared by every Xenon driver.
//!
//! Drivers are blocking: the UI calls them from a background thread, and each
//! driver owns whatever runtime it needs internally. Cancellation goes through
//! a separate [`CancelHandle`] so it can be triggered while a query holds the
//! connection.

pub mod sql;

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

pub use anyhow::{Error, Result};

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
    fn execute(&self, sql: &str, row_limit: usize) -> Result<QueryResult>;

    fn introspect(&self) -> Result<Vec<Schema>>;

    fn cancel_handle(&self) -> Arc<dyn CancelHandle>;

    /// True for a file on this machine or a server on localhost.
    fn is_local(&self) -> bool {
        false
    }

    /// Quote an identifier for this engine's SQL dialect.
    fn quote_ident(&self, ident: &str) -> String {
        format!("\"{}\"", ident.replace('"', "\"\""))
    }
}
