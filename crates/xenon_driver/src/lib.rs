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
    /// Outcomes of the statements before it, in order (a script of several).
    pub earlier: Vec<QueryOutcome>,
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
    /// The values an enum column allows, in order (empty for other types).
    pub options: Vec<String>,
}

/// The values of an enum type written as `enum('a','b')` (MySQL's
/// `column_type`, DuckDB's `ENUM('a', 'b')`); empty for anything else.
pub fn enum_options(type_text: &str) -> Vec<String> {
    let lower = type_text.trim().to_ascii_lowercase();
    if !lower.starts_with("enum(") || !lower.ends_with(')') {
        return Vec::new();
    }
    let inner = &type_text.trim()[5..type_text.trim().len() - 1];
    let mut options = Vec::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\'' {
            continue;
        }
        let mut value = String::new();
        while let Some(c) = chars.next() {
            match c {
                // A doubled quote is a quote; so is a backslash-escaped one.
                '\'' if chars.peek() == Some(&'\'') => {
                    chars.next();
                    value.push('\'');
                }
                '\\' => value.extend(chars.next()),
                '\'' => break,
                c => value.push(c),
            }
        }
        options.push(value);
    }
    options
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
/// engines map them onto their own settings. Ordered from weakest to strictest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
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
#[derive(Clone, Default, PartialEq)]
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
    /// A CA to trust, and a client certificate to log in with (mutual TLS).
    pub tls: TlsFiles,
}

/// Certificate files for TLS, as PEM. Empty for none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TlsFiles {
    /// A CA certificate to trust besides the system's (a private CA).
    pub ca: String,
    /// The client certificate, for servers that want one (mutual TLS).
    pub cert: String,
    /// The client certificate's private key (PKCS#8 or PKCS#1).
    pub key: String,
}

impl TlsFiles {
    pub fn is_empty(&self) -> bool {
        self.ca.is_empty() && self.cert.is_empty() && self.key.is_empty()
    }

    /// Read `path` (a leading `~` is the home folder), saying which file it was.
    pub fn read(path: &str, what: &str) -> anyhow::Result<Vec<u8>> {
        let path = match path.strip_prefix("~/") {
            Some(rest) => std::env::var("HOME")
                .map(|home| format!("{home}/{rest}"))
                .unwrap_or_else(|_| path.to_string()),
            None => path.to_string(),
        };
        std::fs::read(&path)
            .map_err(|err| anyhow::anyhow!("Couldn't read the {what} ({path}): {err}"))
    }

    /// Both halves of a client certificate, or neither.
    pub fn client(&self) -> anyhow::Result<Option<(&str, &str)>> {
        match (self.cert.is_empty(), self.key.is_empty()) {
            (true, true) => Ok(None),
            (false, false) => Ok(Some((&self.cert, &self.key))),
            (false, true) => Err(anyhow::anyhow!(
                "A client certificate needs its key file too."
            )),
            (true, false) => Err(anyhow::anyhow!(
                "A client key needs its certificate file too."
            )),
        }
    }
}

/// An SSH server to tunnel through. Anything left empty comes from
/// `~/.ssh/config` (so `host` can be an alias there) or ssh's own defaults;
/// keys come from the agent or the config unless `key_file` is set.
#[derive(Clone, Default, PartialEq)]
pub struct SshOptions {
    pub host: String,
    pub port: Option<u16>,
    pub user: String,
    pub key_file: String,
    /// The SSH password, or the key's passphrase. Kept in the Keychain.
    pub password: String,
}

/// Whether a secret is set, for `Debug`: never the secret itself.
fn redacted(secret: &str) -> &'static str {
    if secret.is_empty() { "" } else { "<redacted>" }
}

impl std::fmt::Debug for ConnectOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectOptions")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("database", &self.database)
            .field("user", &self.user)
            .field("password", &redacted(&self.password))
            .field("ssl", &self.ssl)
            .field("read_only", &self.read_only)
            .field("ssh", &self.ssh)
            .field("address", &self.address)
            .field("lock_timeout", &self.lock_timeout)
            .field("tls", &self.tls)
            .finish()
    }
}

impl std::fmt::Debug for SshOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshOptions")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("key_file", &self.key_file)
            .field("password", &redacted(&self.password))
            .finish()
    }
}

/// A private key as PKCS#8 PEM, which native-TLS wants: PKCS#8 as is, an RSA
/// key in PKCS#1 (`BEGIN RSA PRIVATE KEY`) wrapped. Other kinds are refused
/// with the command that converts them.
pub fn pkcs8_key(pem_bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    let block =
        pem::parse(pem_bytes).map_err(|_| anyhow::anyhow!("The client key isn't a PEM key."))?;
    match block.tag() {
        "PRIVATE KEY" => Ok(pem_bytes.to_vec()),
        "RSA PRIVATE KEY" => {
            // PrivateKeyInfo { version 0, rsaEncryption, OCTET STRING <PKCS#1> }.
            let algorithm = [
                0x30, 0x0d, 0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01, 0x05,
                0x00,
            ];
            let mut body = vec![0x02, 0x01, 0x00];
            body.extend_from_slice(&algorithm);
            body.extend(der_wrap(0x04, block.contents()));
            let der = der_wrap(0x30, &body);
            Ok(pem::encode(&pem::Pem::new("PRIVATE KEY", der)).into_bytes())
        }
        other => Err(anyhow::anyhow!(
            "The client key is a {other}; Xenon reads PKCS#8 and RSA keys. Convert it with \
             `openssl pkcs8 -topk8 -nocrypt -in key.pem -out key-pkcs8.pem`."
        )),
    }
}

/// A DER element: `tag`, its length, `contents`.
fn der_wrap(tag: u8, contents: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let len = contents.len();
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let bytes: Vec<u8> = len
            .to_be_bytes()
            .into_iter()
            .skip_while(|b| *b == 0)
            .collect();
        out.push(0x80 | bytes.len() as u8);
        out.extend(bytes);
    }
    out.extend_from_slice(contents);
    out
}

/// [`Connection::unchanged`] for a column `=` compares exactly (and floats
/// within one part in a million), for drivers to fall back on.
pub fn default_unchanged<C: Connection + ?Sized>(
    conn: &C,
    column: &str,
    loaded: &Value,
) -> Option<String> {
    Some(match loaded {
        Value::Null => format!("{column} is null"),
        Value::Real(n) if !n.is_finite() => return None,
        // Within a millionth of its size, rounded to a power of ten so the SQL
        // reads plainly: `abs(total - 10.5) <= 0.00001`.
        Value::Real(n) => {
            let scale = if *n == 0. {
                -12
            } else {
                n.abs().log10().ceil() as i32 - 6
            };
            let margin = 10f64.powi(scale);
            format!("abs({column} - {}) <= {margin}", conn.literal(loaded))
        }
        value => format!("{column} = {}", conn.literal(value)),
    })
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

    /// What tells this server apart from another answering at the same
    /// address, read when it connected (Postgres's system identifier, MySQL's
    /// server UUID). `None` when the engine has none, or it couldn't be read.
    fn server_identity(&self) -> Option<String> {
        None
    }

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

    /// A condition that holds only while `column` (already quoted) still has
    /// `loaded`, the value read when the rows were fetched: saving an edit or
    /// delete with it changes nothing if someone else changed the row since.
    /// `None` when the engine can't compare a column of `type_name` exactly.
    /// Floating-point numbers match within one part in a million (MySQL's
    /// `float` keeps fewer digits than are shown).
    fn unchanged(&self, column: &str, type_name: &str, loaded: &Value) -> Option<String> {
        let _ = type_name;
        default_unchanged(self, column, loaded)
    }

    /// Whether the server is still there: true when it answers, even with an
    /// error (a statement inside a failed transaction is refused, but the
    /// connection is fine); false when the connection is gone. May block as
    /// long as the network does, so callers put a time limit on it.
    fn alive(&self) -> bool {
        true
    }

    /// Whether a transaction the user began is still open (so its locks are
    /// held until commit or rollback). `None` when the engine can't say.
    fn in_transaction(&self) -> Option<bool> {
        None
    }

    /// True when the session that ran the last statement has ended and the
    /// next query opens another (SQL Server after a cancel or a drop): what it
    /// was doing is unknown, even though the connection answers.
    fn session_ended(&self) -> bool {
        false
    }

    /// True when a `use` has made another database current than the one
    /// opened ([`Connection::catalog`]), so what introspection says (a table's
    /// key) may describe another database's table of the same name.
    fn switched_catalog(&self) -> bool {
        false
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

    /// The database (catalog) the session opened on, where a `USE` could
    /// switch it (SQL Server, DuckDB): named in generated SQL so a browse or a
    /// save can't land on another database's table of the same name.
    fn catalog(&self) -> Option<String> {
        None
    }
}

/// `table` in `schema` (the connection's default schema when `None`), named in
/// full, with its catalog where the engine has one that can change: the name
/// browses, counts and saves use, so `USE other` or a changed `search_path` in
/// some tab can't send them to another table of the same name.
pub fn qualified_name(conn: &dyn Connection, schema: Option<&str>, table: &str) -> String {
    let schema = schema.map(str::to_string).or_else(|| conn.default_schema());
    conn.catalog()
        .into_iter()
        .chain(schema)
        .map(|part| conn.ident(&part))
        .chain([conn.ident(table)])
        .collect::<Vec<_>>()
        .join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_never_shows_a_password() {
        let options = ConnectOptions {
            password: "db-secret".into(),
            ssh: Some(SshOptions {
                password: "ssh-secret".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let shown = format!("{options:?}");
        assert!(!shown.contains("secret"), "{shown}");
        assert!(shown.contains("<redacted>"));
    }

    #[test]
    fn rsa_keys_convert_to_pkcs8_as_openssl_does() {
        // A throwaway 1024-bit test key, and `openssl pkcs8 -topk8 -nocrypt` of it.
        let pkcs1 = include_bytes!("../tests/keys/rsa-pkcs1.pem");
        let expected =
            pem::parse(include_bytes!("../tests/keys/rsa-pkcs8.pem").as_slice()).unwrap();
        let converted = pem::parse(pkcs8_key(pkcs1).unwrap()).unwrap();
        assert_eq!(converted.tag(), "PRIVATE KEY");
        assert_eq!(converted.contents(), expected.contents());
        // PKCS#8 passes through; anything else says how to convert it.
        let pkcs8 = include_bytes!("../tests/keys/rsa-pkcs8.pem");
        assert_eq!(pkcs8_key(pkcs8).unwrap(), pkcs8.to_vec());
        let ec = b"-----BEGIN EC PRIVATE KEY-----\nAAAA\n-----END EC PRIVATE KEY-----\n";
        assert!(
            pkcs8_key(ec)
                .unwrap_err()
                .to_string()
                .contains("openssl pkcs8")
        );
    }

    #[test]
    fn client_certificates_need_both_halves() {
        let files = |cert: &str, key: &str| TlsFiles {
            ca: String::new(),
            cert: cert.into(),
            key: key.into(),
        };
        assert_eq!(files("", "").client().unwrap(), None);
        assert_eq!(
            files("a.pem", "a.key").client().unwrap(),
            Some(("a.pem", "a.key"))
        );
        assert!(files("a.pem", "").client().is_err());
        assert!(files("", "a.key").client().is_err());
    }

    #[test]
    fn enum_options_from_type_text() {
        assert_eq!(
            enum_options("enum('paid','refunded')"),
            ["paid", "refunded"]
        );
        assert_eq!(
            enum_options("ENUM('a', 'it''s', 'b\\'c')"),
            ["a", "it's", "b'c"]
        );
        assert!(enum_options("varchar(20)").is_empty());
    }
}
