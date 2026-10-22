//! MySQL and MariaDB driver, built on the synchronous `mysql` crate with native TLS.
//!
//! Queries use the text protocol, so values arrive as the server prints them
//! (exact decimals, dates, JSON) and multi-statement scripts work. Column types
//! come with every result set and decide how each value is shown.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use mysql::consts::{ColumnFlags, ColumnType};
use mysql::prelude::Queryable as _;
use mysql::{Conn, Opts, OptsBuilder, SslOpts};
use xenon_driver::{
    CancelHandle, Column, ColumnInfo, Connection, Feed, ForeignKey, IndexInfo, ObjectColumn,
    ObjectKind, Progress, QueryError, QueryOutcome, QueryResult, ResultSet, Schema, SchemaObject,
    TableInfo, TlsFiles, Value, referential_action,
};
pub use xenon_driver::{ConnectOptions, SslMode};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_PORT: u16 = 3306;
/// MySQL's "binary" character set: columns in it hold bytes, not text.
const BINARY_CHARSET: u16 = 63;
/// "Query execution was interrupted": what a `KILL QUERY` looks like to the victim.
const ER_QUERY_INTERRUPTED: u16 = 1317;
const SYSTEM_SCHEMAS: [&str; 4] = ["information_schema", "mysql", "performance_schema", "sys"];

/// Split a `mysql://` (or `mariadb://`) URL into fields, so it can be saved
/// with the password kept apart. Understands `ssl-mode` / `sslmode` query values.
pub fn parse_url(url: &str) -> Result<ConnectOptions> {
    let rest = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .ok_or_else(|| anyhow!("not a mysql:// URL"))?;
    let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (authority, database) = rest.split_once('/').unwrap_or((rest, ""));
    let (credentials, host_port) = match authority.rsplit_once('@') {
        Some((credentials, host_port)) => (Some(credentials), host_port),
        None => (None, authority),
    };
    let (user, password) = match credentials {
        Some(credentials) => match credentials.split_once(':') {
            Some((user, password)) => (decode(user), decode(password)),
            None => (decode(credentials), String::new()),
        },
        None => (String::new(), String::new()),
    };
    let (host, port) = match host_port.rsplit_once(':') {
        Some((host, port)) if !host.ends_with(']') || host_port.starts_with('[') => (
            host.trim_matches(['[', ']']).to_string(),
            Some(port.parse::<u16>().context("invalid port")?),
        ),
        _ => (host_port.trim_matches(['[', ']']).to_string(), None),
    };
    let mut ssl: Option<SslMode> = None;
    let mut tls = TlsFiles::default();
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let truthy = || {
            matches!(
                decode(value).to_ascii_lowercase().as_str(),
                "true" | "1" | "yes"
            )
        };
        let mut asked = |mode: SslMode| ssl = Some(ssl.map_or(mode, |earlier| earlier.max(mode)));
        match key.to_ascii_lowercase().replace('_', "-").as_str() {
            "ssl-ca" | "sslca" => tls.ca = decode(value),
            "ssl-cert" | "sslcert" => tls.cert = decode(value),
            "ssl-key" | "sslkey" => tls.key = decode(value),
            "ssl-mode" | "sslmode" => {}
            // JDBC's and others' spellings.
            "usessl" | "ssl" | "requiressl" => asked(if truthy() {
                SslMode::Require
            } else {
                SslMode::Disable
            }),
            "verifyservercertificate" | "ssl-verify-server-cert" => {
                if truthy() {
                    asked(SslMode::VerifyFull)
                }
            }
            // Anything else about TLS can't be followed: refused, not dropped.
            other
                if ["ssl", "tls", "cert"]
                    .iter()
                    .any(|word| other.contains(word)) =>
            {
                bail!(
                    "Xenon doesn't support the TLS option \"{key}\" in a MySQL URL: use ssl-mode, ssl-ca, ssl-cert and ssl-key."
                )
            }
            _ => {}
        }
        if matches!(
            key.to_ascii_lowercase().as_str(),
            "ssl-mode" | "sslmode" | "ssl_mode"
        ) {
            // Spelled any way MySQL's clients and libpq's do; anything else is
            // refused rather than quietly read as `prefer`.
            let mode = match decode(value)
                .to_ascii_lowercase()
                .replace('_', "-")
                .as_str()
            {
                "disabled" | "disable" | "false" => SslMode::Disable,
                "preferred" | "prefer" => SslMode::Prefer,
                "required" | "require" | "true" => SslMode::Require,
                "verify-ca" => SslMode::VerifyCa,
                "verify-identity" | "verify-full" => SslMode::VerifyFull,
                other => bail!(
                    "Unknown ssl-mode \"{other}\": use disabled, preferred, required, verify_ca or verify_identity."
                ),
            };
            // Given twice, the stricter holds.
            ssl = Some(ssl.map_or(mode, |earlier| earlier.max(mode)));
        }
    }
    Ok(ConnectOptions {
        host,
        port,
        database: decode(database),
        user,
        password,
        // A CA means checking against it, as MySQL's client does.
        ssl: match ssl {
            None | Some(SslMode::Require) if !tls.ca.is_empty() => SslMode::VerifyCa,
            ssl => ssl.unwrap_or_default(),
        },
        read_only: false,
        ssh: None,
        address: None,
        lock_timeout: None,
        tls,
    })
}

/// A client certificate bundled as a PKCS#12 file (owner-only, in the temp
/// folder), which native-TLS needs; deleted when the connection is.
struct Identity {
    path: std::path::PathBuf,
}

const IDENTITY_PASSWORD: &str = "xenon";

impl Identity {
    fn bundle(cert: &str, key: &str) -> Result<Self> {
        let der = |bytes: Vec<u8>, what: &str| -> Result<Vec<u8>> {
            Ok(pem::parse(bytes)
                .map_err(|_| anyhow!("The {what} isn't PEM."))?
                .into_contents())
        };
        let cert = der(
            TlsFiles::read(cert, "client certificate")?,
            "client certificate",
        )?;
        let key = der(
            xenon_driver::pkcs8_key(&TlsFiles::read(key, "client key")?)?,
            "client key",
        )?;
        let bundle_error = |_| anyhow!("Couldn't bundle the client certificate and key.");
        let mut store = p12_keystore::KeyStore::new();
        store.add_entry(
            "xenon",
            p12_keystore::KeyStoreEntry::PrivateKeyChain(p12_keystore::PrivateKeyChain::new(
                b"xenon".as_slice(),
                p12_keystore::PrivateKey::from_der(&key).map_err(bundle_error)?,
                [p12_keystore::Certificate::from_der(&cert).map_err(bundle_error)?],
            )),
        );
        // Triple DES and a SHA-1 MAC: what both OpenSSL 3 and macOS read (not
        // the RC2 some tools still write, which OpenSSL 3 refuses).
        let der = store
            .writer(IDENTITY_PASSWORD)
            .encryption_algorithm(p12_keystore::EncryptionAlgorithm::PbeWithShaAnd3KeyTripleDesCbc)
            .mac_algorithm(p12_keystore::MacAlgorithm::HmacSha1)
            .write()
            .map_err(bundle_error)?;
        let path = std::env::temp_dir().join(format!(
            "xenon-identity-{}-{}.p12",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?
            .write_all(&der)?;
        Ok(Self { path })
    }
}

impl Drop for Identity {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn expand_home(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => {
            std::env::var("HOME").map_or_else(|_| path.to_string(), |h| format!("{h}/{rest}"))
        }
        None => path.to_string(),
    }
}

/// Undo `%xx` escapes in a URL part.
fn decode(part: &str) -> String {
    let bytes = part.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = part
                .get(i + 1..i + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub struct MysqlConnection {
    name: String,
    id: String,
    local: bool,
    version: String,
    server: Option<String>,
    /// The database connected to, if any: where unqualified names resolve.
    schema: Option<String>,
    mariadb: bool,
    read_only: bool,
    conn: Mutex<Conn>,
    cancel: Arc<MyCancel>,
    /// Whether a transaction is open, and whether autocommit is off, as
    /// followed from what ran (see `in_transaction`).
    transaction: Mutex<(bool, bool)>,
}

/// Cancels by asking the server, over a second connection, to kill the query
/// the main connection is running.
struct MyCancel {
    opts: Opts,
    /// The client certificate `opts` points at, kept while it's needed.
    _identity: Option<Arc<Identity>>,
    connection_id: u32,
    /// Set when the user cancels (not the row limit), so the rows read so far are kept.
    user: std::sync::atomic::AtomicBool,
    /// A user's kill on its way: the query waits for it to land before
    /// returning, so it can't stop what runs next instead.
    pending: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl CancelHandle for MyCancel {
    fn cancel(&self) {
        self.user.store(true, std::sync::atomic::Ordering::SeqCst);
        let kill = self.stop();
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(kill);
    }
}

impl MyCancel {
    /// Stop the running query from another thread, without waiting.
    fn stop(&self) -> std::thread::JoinHandle<()> {
        let (opts, id) = (self.opts.clone(), self.connection_id);
        std::thread::spawn(move || kill_query(opts, id))
    }

    /// Stop the running query and wait until the server has, so the kill can't
    /// land on the next query instead (MySQL clears a kill that finds the
    /// connection idle before its next statement).
    fn stop_now(&self) {
        kill_query(self.opts.clone(), self.connection_id);
    }
}

fn kill_query(opts: Opts, id: u32) {
    // Nothing to report if the kill doesn't land: the query just runs on.
    if let Ok(mut conn) = Conn::new(opts) {
        let _ = conn.query_drop(format!("KILL QUERY {id}"));
    }
}

impl MysqlConnection {
    pub fn connect_url(url: &str) -> Result<Self> {
        Self::connect_options(&parse_url(url)?)
    }

    pub fn connect_options(options: &ConnectOptions) -> Result<Self> {
        let host = if options.host.is_empty() {
            "localhost"
        } else {
            &options.host
        };
        let port = options.port.unwrap_or(DEFAULT_PORT);
        // Through a tunnel, connect to its local end. (The mysql crate checks
        // TLS names against what it connects to, so verify-full can't pass here.)
        let (connect_host, connect_port) = match options.address {
            Some(address) => (address.ip().to_string(), address.port()),
            None => (host.to_string(), port),
        };
        let user = if options.user.is_empty() {
            "root"
        } else {
            &options.user
        };
        let mut init = vec!["SET NAMES utf8mb4".to_string()];
        if options.read_only {
            init.push("SET SESSION TRANSACTION READ ONLY".to_string());
        }
        if let Some(wait) = options.lock_timeout {
            // Row locks (InnoDB) and table locks (metadata, e.g. behind an ALTER); whole seconds.
            let seconds = wait.as_secs().max(1);
            init.push(format!("SET SESSION innodb_lock_wait_timeout = {seconds}"));
            init.push(format!("SET SESSION lock_wait_timeout = {seconds}"));
        }
        let builder = OptsBuilder::new()
            .ip_or_hostname(Some(connect_host))
            .tcp_port(connect_port)
            .user(Some(user))
            .pass((!options.password.is_empty()).then(|| options.password.clone()))
            .db_name((!options.database.is_empty()).then(|| options.database.clone()))
            .tcp_connect_timeout(Some(CONNECT_TIMEOUT))
            // Over TCP, always: a "localhost" socket file is rarely where Docker puts it.
            .prefer_socket(false)
            // Report rows an UPDATE matched, not only those it changed, so saving
            // an edit that sets a value to itself still counts as one row.
            .additional_capabilities(mysql::consts::CapabilityFlags::CLIENT_FOUND_ROWS)
            .init(init);

        let identity = match options.tls.client()? {
            Some((cert, key)) => Some(Arc::new(Identity::bundle(cert, key)?)),
            None => None,
        };
        let ca = (!options.tls.ca.is_empty()).then(|| {
            // Read it here, so a missing file says so (not a TLS error).
            TlsFiles::read(&options.tls.ca, "CA certificate").map(|_| options.tls.ca.clone())
        });
        let ca = ca.transpose()?;
        let tls = |mode: SslMode| -> Option<SslOpts> {
            let base = SslOpts::default()
                .with_root_cert_path(
                    ca.clone()
                        .map(|p| std::path::PathBuf::from(expand_home(&p))),
                )
                .with_client_identity(identity.as_ref().map(|i| {
                    mysql::ClientIdentity::new(i.path.clone()).with_password(IDENTITY_PASSWORD)
                }));
            match mode {
                SslMode::Disable => None,
                SslMode::Prefer | SslMode::Require => Some(
                    base.with_danger_accept_invalid_certs(true)
                        .with_danger_skip_domain_validation(true),
                ),
                SslMode::VerifyCa => Some(base.with_danger_skip_domain_validation(true)),
                SslMode::VerifyFull => Some(base),
            }
        };
        let opts: Opts = builder.clone().ssl_opts(tls(options.ssl)).into();
        let (conn, opts) = match Conn::new(opts.clone()) {
            Ok(conn) => (conn, opts),
            // `prefer`: fall back to plain TCP when the server won't do TLS, and
            // only on this Mac. Anywhere else, whatever answers could be posing
            // as the server and get the password sent without TLS: there, say so.
            Err(mysql::Error::DriverError(mysql::DriverError::TlsNotSupported))
                if options.ssl == SslMode::Prefer =>
            {
                // Through an SSH tunnel, SSH encrypts it.
                if !matches!(host, "localhost" | "127.0.0.1" | "::1") && options.address.is_none() {
                    return Err(anyhow!(
                        "{host} doesn't offer TLS, so Xenon didn't send the password: anything \
                         on the network could read it. To connect without TLS anyway, set \
                         ssl-mode=disabled."
                    ));
                }
                let plain: Opts = builder.ssl_opts(None::<SslOpts>).into();
                (Conn::new(plain.clone()).map_err(describe_error)?, plain)
            }
            Err(err) => return Err(describe_error(err)),
        };

        let mut conn = conn;
        let schema: Option<String> = conn
            .query_first("select database()")
            .map_err(describe_error)?
            .flatten();
        let raw_version: String = conn
            .query_first("select version()")
            .map_err(describe_error)?
            .unwrap_or_default();
        let mariadb = raw_version.to_ascii_lowercase().contains("mariadb");
        let autocommit_off = conn
            .query_first::<i64, _>("select @@autocommit")
            .ok()
            .flatten()
            == Some(0);
        // MariaDB has no server UUID: its host name, then.
        let server = conn
            .query_first::<String, _>("select @@server_uuid")
            .ok()
            .flatten()
            .or_else(|| {
                conn.query_first::<String, _>("select @@hostname")
                    .ok()
                    .flatten()
            });
        let number = raw_version
            .split(['-', ' '])
            .next()
            .unwrap_or("")
            .to_string();
        let database = options.database.clone();
        let name = if database.is_empty() {
            format!("{user}@{host}")
        } else {
            format!("{database}@{host}")
        };
        Ok(Self {
            id: format!("mysql:{user}@{host}:{port}/{database}"),
            local: matches!(host, "localhost" | "127.0.0.1" | "::1"),
            version: format!("{} {number}", if mariadb { "mariadb" } else { "mysql" }),
            server,
            mariadb,
            schema,
            read_only: options.read_only,
            cancel: Arc::new(MyCancel {
                opts,
                _identity: identity,
                connection_id: conn.connection_id(),
                user: Default::default(),
                pending: Default::default(),
            }),
            conn: Mutex::new(conn),
            transaction: Mutex::new((autocommit_off, autocommit_off)),
            name,
        })
    }

    /// Wait for a user's kill still on its way, so it can't stop what runs next.
    fn settle_kill(&self) {
        let pending = self
            .cancel
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        // A kill connection that never gets an answer mustn't hang the
        // session: give it a few seconds, then leave it to finish on its own.
        if let Some(pending) = pending {
            let started = std::time::Instant::now();
            while !pending.is_finished() && started.elapsed() < std::time::Duration::from_secs(5) {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            if pending.is_finished() {
                let _ = pending.join();
            }
        }
    }

    /// True when the server is MariaDB rather than MySQL.
    pub fn is_mariadb(&self) -> bool {
        self.mariadb
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Conn> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Connection for MysqlConnection {
    fn name(&self) -> &str {
        &self.name
    }

    fn id(&self) -> String {
        self.id.clone()
    }

    fn server_version(&self) -> String {
        self.version.clone()
    }

    fn server_identity(&self) -> Option<String> {
        self.server.clone()
    }

    fn execute_streaming(
        &self,
        sql: &str,
        row_limit: usize,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<QueryResult> {
        // A kill pressed just after the last query returned: let it land first.
        self.settle_kill();
        let result = (|| -> Result<QueryResult> {
            // MySQL's read-only session stops DML but not DDL (which commits on its
            // own), nor DDL run by `prepare … execute` or a procedure: refuse
            // anything recognised as a write here, so read-only means read-only.
            if self.read_only
                && let Some((_, what)) = xenon_driver::sql::assess(sql)
            {
                return Err(QueryError {
                    message: format!("This is a read-only connection, so {what} isn't allowed."),
                    position: None,
                }
                .into());
            }

            if self.read_only && xenon_driver::sql::lifts_read_only(sql) {
                return Err(QueryError {
                    message:
                        "This is a read-only connection, so switching it to read-write isn't allowed."
                            .into(),
                    position: None,
                }
                .into());
            }
            // A write must finish, and so must a transaction (a kill would stop
            // a script before its `commit`): the row limit reads on in both.
            // So must a script's later statements, which a kill would stop.
            let writes = xenon_driver::sql::writes(sql)
                || xenon_driver::sql::several_statements(sql)
                || xenon_driver::sql::controls_transaction(sql)
                || self.transaction.lock().unwrap_or_else(|e| e.into_inner()).0;
            let started = Instant::now();
            let mut feed = Feed::new(progress);
            let mut cancelled = false;
            self.cancel
                .user
                .store(false, std::sync::atomic::Ordering::SeqCst);
            let mut conn = self.lock();
            let mut result = conn
                .query_iter(sql)
                .map_err(|err| locate(describe_error(err), sql))?;

            let mut statements = 0;
            let mut outcome = None;
            let mut earlier = Vec::new();
            let mut cancelled_for_limit = false;
            while let Some(mut set) = result.iter() {
                statements += 1;
                let columns: Vec<mysql::Column> = set.columns().as_ref().to_vec();
                let affected = set.affected_rows();
                if columns.is_empty() {
                    // A statement after the first that failed arrives as an empty
                    // set holding the error: read it, or the error is lost (and a
                    // production script's earlier statements committed).
                    if let Some(Err(err)) = set.next() {
                        return Err(locate(describe_error(err), sql));
                    }
                    earlier.extend(outcome.replace(QueryOutcome::Affected(affected)));
                    continue;
                }
                let mut rows = ResultSet {
                    columns: columns
                        .iter()
                        .map(|c| Column {
                            name: c.name_str().into_owned(),
                            type_name: Some(type_label(c)),
                        })
                        .collect(),
                    ..Default::default()
                };
                feed.next_set();
                for row in set {
                    let row = match row {
                        Ok(row) => row,
                        Err(mysql::Error::MySqlError(err))
                            if cancelled_for_limit && err.code == ER_QUERY_INTERRUPTED =>
                        {
                            break;
                        }
                        // Stopped by the user while reading rows: keep what arrived.
                        Err(mysql::Error::MySqlError(err))
                            if err.code == ER_QUERY_INTERRUPTED
                                && self.cancel.user.load(std::sync::atomic::Ordering::SeqCst)
                                && !rows.rows.is_empty() =>
                        {
                            cancelled = true;
                            rows.truncated = true;
                            break;
                        }
                        Err(err) => return Err(locate(describe_error(err), sql)),
                    };
                    if rows.rows.len() == row_limit {
                        if !rows.truncated {
                            // Stop the server instead of reading rows we'd discard.
                            rows.truncated = true;
                            // A write must finish (a kill would roll it back), so its
                            // extra rows are read and dropped instead.
                            if !writes {
                                cancelled_for_limit = true;
                                self.cancel.stop_now();
                            }
                        }
                        continue;
                    }
                    rows.rows.push(
                        row.unwrap()
                            .into_iter()
                            .zip(&columns)
                            .map(|(value, column)| convert(value, column))
                            .collect(),
                    );
                    feed.read(&rows, |_| {});
                }
                earlier.extend(outcome.replace(QueryOutcome::Rows(rows)));
                if cancelled {
                    break;
                }
            }
            drop(result);
            let outcome = outcome.ok_or_else(|| anyhow!("nothing to run"))?;
            // Some killed statements (`sleep()`, `benchmark()`) return a value instead
            // of failing; after a cancel, that value isn't the answer.
            let cancelled = cancelled || self.cancel.user.load(std::sync::atomic::Ordering::SeqCst);
            Ok(QueryResult {
                outcome,
                earlier,
                statements,
                elapsed: started.elapsed(),
                cancelled,
            })
        })();
        // A kill sent while it ran: wait for it to land (MySQL drops one that
        // finds the connection idle) before anything else runs here.
        self.settle_kill();
        // However it was done, a read-only session switched to read-write goes
        // straight back, also when the script then failed.
        let mut result = result;
        if self.read_only
            && self
                .lock()
                .query_first::<i64, _>("select @@session.transaction_read_only")
                .ok()
                .flatten()
                == Some(0)
        {
            let _ = self.lock().query_drop("set session transaction read only");
            result = Err(QueryError {
                message: "This read-only connection was switched to read-write; Xenon switched it \
                          back."
                    .into(),
                position: None,
            }
            .into());
        }
        // Follow the transaction: MySQL can't be asked. A failed script may have
        // opened one before failing, so then it counts as open.
        let mut state = self.transaction.lock().unwrap_or_else(|e| e.into_inner());
        let (open, autocommit_off) =
            xenon_driver::sql::mysql_transaction_state(sql, state.0, state.1);
        *state = match &result {
            Ok(_) => (open, autocommit_off),
            // Failed part-way: what followed the failure (a closing `commit`,
            // say) didn't run, so one opened on the way is still open.
            Err(_) => (
                xenon_driver::sql::mysql_transaction_open_after_failure(sql, state.0, state.1),
                state.1 || autocommit_off,
            ),
        };
        result
    }

    fn introspect(&self) -> Result<Vec<Schema>> {
        let query = format!(
            "select c.table_schema, c.table_name, t.table_type, c.column_name, c.column_type,
                    c.is_nullable = 'NO', c.column_key = 'PRI'
             from information_schema.columns c
             join information_schema.tables t
               on t.table_schema = c.table_schema and t.table_name = c.table_name
             where c.table_schema not in ({})
             order by c.table_schema = database() desc, c.table_schema, c.table_name, c.ordinal_position",
            SYSTEM_SCHEMAS.map(|s| format!("'{s}'")).join(", ")
        );
        type Row = (String, String, String, String, String, bool, bool);
        let rows: Vec<Row> = self.lock().query(query).map_err(describe_error)?;

        let mut schemas: Vec<Schema> = Vec::new();
        for (schema, table, kind, column, type_name, not_null, primary_key) in rows {
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
                    kind: if kind == "VIEW" {
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
                .push(ObjectColumn {
                    name: column,
                    type_name,
                    not_null,
                    primary_key,
                });
        }
        Ok(schemas)
    }

    fn describe(&self, schema: &str, table: &str) -> Result<TableInfo> {
        const COLUMNS: &str = "
            select column_name, column_type, is_nullable = 'YES', column_default, extra,
                   column_key = 'PRI', column_comment
            from information_schema.columns
            where table_schema = ? and table_name = ?
            order by ordinal_position";
        const INDEXES: &str = "
            select index_name, non_unique = 0, coalesce(column_name, '(expression)')
            from information_schema.statistics
            where table_schema = ? and table_name = ?
            order by index_name = 'PRIMARY' desc, index_name, seq_in_index";
        const KEYS: &str = "
            select k.constraint_name, k.table_schema, k.table_name, k.column_name,
                   k.referenced_table_schema, k.referenced_table_name, k.referenced_column_name,
                   r.delete_rule, r.update_rule
            from information_schema.key_column_usage k
            join information_schema.referential_constraints r
              on r.constraint_schema = k.constraint_schema
             and r.constraint_name = k.constraint_name
             and r.table_name = k.table_name
            where k.referenced_table_name is not null
              and ((k.table_schema = ? and k.table_name = ?)
                or (k.referenced_table_schema = ? and k.referenced_table_name = ?))
            order by k.table_schema, k.table_name, k.constraint_name, k.ordinal_position";

        let mut conn = self.lock();
        let mut info = TableInfo::default();
        type ColumnRow = (String, String, bool, Option<String>, String, bool, String);
        let columns: Vec<ColumnRow> = conn
            .exec(COLUMNS, (schema, table))
            .map_err(describe_error)?;
        for (name, type_name, nullable, default, extra, primary_key, comment) in columns {
            // `auto_increment`, `on update CURRENT_TIMESTAMP`, `VIRTUAL GENERATED`…
            let extra = extra.replace("DEFAULT_GENERATED", "").trim().to_string();
            let default = match (default, extra.is_empty()) {
                (Some(default), true) => Some(default),
                (Some(default), false) => Some(format!("{default} {extra}")),
                (None, false) => Some(extra),
                (None, true) => None,
            };
            info.columns.push(ColumnInfo {
                options: xenon_driver::enum_options(&type_name),
                name,
                type_name,
                nullable,
                default,
                primary_key,
                comment: (!comment.is_empty()).then_some(comment),
            });
        }
        let indexes: Vec<(String, bool, String)> = conn
            .exec(INDEXES, (schema, table))
            .map_err(describe_error)?;
        for (name, unique, column) in indexes {
            match info.indexes.last_mut() {
                Some(last) if last.name == name => last.columns.push(column),
                _ => info.indexes.push(IndexInfo {
                    primary: name == "PRIMARY",
                    name,
                    unique,
                    columns: vec![column],
                }),
            }
        }
        type KeyRow = (
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            String,
        );
        let keys: Vec<KeyRow> = conn
            .exec(KEYS, (schema, table, schema, table))
            .map_err(describe_error)?;
        let mut all: Vec<ForeignKey> = Vec::new();
        for (name, from_schema, from_table, column, to_schema, to_table, to_column, del, upd) in
            keys
        {
            match all.last_mut() {
                Some(last)
                    if last.name == name
                        && last.schema == from_schema
                        && last.table == from_table =>
                {
                    last.columns.push(column);
                    last.ref_columns.push(to_column);
                }
                _ => all.push(ForeignKey {
                    name,
                    schema: from_schema,
                    table: from_table,
                    columns: vec![column],
                    ref_schema: to_schema,
                    ref_table: to_table,
                    ref_columns: vec![to_column],
                    on_delete: referential_action(&del),
                    on_update: referential_action(&upd),
                }),
            }
        }
        for key in all {
            if key.ref_schema == schema && key.ref_table == table {
                info.referenced_by.push(key.clone());
            }
            if key.schema == schema && key.table == table {
                info.foreign_keys.push(key);
            }
        }
        info.estimated_rows = conn
            .exec_first::<Option<u64>, _, _>(
                "select table_rows from information_schema.tables \
                 where table_schema = ? and table_name = ?",
                (schema, table),
            )
            .ok()
            .flatten()
            .flatten();
        let create = format!(
            "show create table {}.{}",
            self.quote_ident(schema),
            self.quote_ident(table)
        );
        info.definition = conn
            .query_first::<mysql::Row, _>(create)
            .ok()
            .flatten()
            .and_then(|row| row.get_opt::<String, _>(1)?.ok());
        Ok(info)
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        self.cancel.clone()
    }

    fn begin_read_only(&self) -> Option<&'static str> {
        Some("start transaction read only")
    }

    fn unchanged(&self, column: &str, type_name: &str, loaded: &Value) -> Option<String> {
        let kind = type_name.to_ascii_lowercase();
        if loaded.is_null() {
            return Some(format!("{column} is null"));
        }
        // `float unsigned` is still a float.
        match kind.trim_end_matches(" unsigned") {
            // By meaning, not spacing or key order.
            "json" => Some(format!("{column} = cast({} as json)", self.literal(loaded))),
            // Spatial values come back in a form that doesn't compare reliably.
            "geometry" | "point" | "linestring" | "polygon" | "multipoint" | "multilinestring"
            | "multipolygon" | "geometrycollection" | "geomcollection" => None,
            // Read back as exact text, but `float` keeps single precision: `f = 0.1`
            // never matches. Compare as the floating-point number it is.
            "float" | "double" | "real" => match loaded {
                Value::Decimal(text) | Value::Text(text) => match text.parse::<f64>() {
                    Ok(n) => xenon_driver::default_unchanged(self, column, &Value::Real(n)),
                    Err(_) => None,
                },
                _ => xenon_driver::default_unchanged(self, column, loaded),
            },
            // The default collations ignore case and accents, and PAD SPACE
            // trailing spaces: compare the characters exactly, and the length.
            "char" | "varchar" | "text" | "enum" | "set" => {
                let literal = self.literal(loaded);
                Some(format!(
                    "convert({column} using utf8mb4) collate utf8mb4_bin = convert({literal} using utf8mb4) collate utf8mb4_bin \
                     and char_length({column}) = char_length({literal})"
                ))
            }
            _ => xenon_driver::default_unchanged(self, column, loaded),
        }
    }

    fn alive(&self) -> bool {
        self.settle_kill();
        match self.lock().query_drop("select 1") {
            Ok(()) => true,
            // The server answered, if only to refuse.
            Err(mysql::Error::MySqlError(_)) => true,
            Err(_) => false,
        }
    }

    fn in_transaction(&self) -> Option<bool> {
        self.settle_kill();
        // MySQL has no reliable way to ask (`information_schema.innodb_trx` is a
        // snapshot refreshed every 100 ms, and needs PROCESS): the driver follows
        // what ran instead, autocommit and implicit commits included. MariaDB can say.
        if !self.mariadb {
            return Some(self.transaction.lock().unwrap_or_else(|e| e.into_inner()).0);
        }
        self.lock()
            .query_first::<i64, _>("select @@in_transaction")
            .ok()
            .flatten()
            .map(|open| open != 0)
    }

    fn is_local(&self) -> bool {
        self.local
    }

    fn quote_ident(&self, ident: &str) -> String {
        format!("`{}`", ident.replace('`', "``"))
    }

    fn default_schema(&self) -> Option<String> {
        self.schema.clone()
    }

    fn literal(&self, value: &Value) -> String {
        match value {
            // A backslash is an escape in MySQL strings, unless the session's
            // `NO_BACKSLASH_ESCAPES` says it isn't, and that can change after
            // connecting (or the SQL can be run elsewhere). Text with one is
            // written as its UTF-8 bytes, which mean the same either way; other
            // text needs only its quotes doubled, which also works either way.
            Value::Text(text) if text.contains('\\') => {
                format!("_utf8mb4 X'{}'", xenon_driver::hex(text.as_bytes()))
            }
            Value::Text(text) => format!("'{}'", text.replace('\'', "''")),
            Value::Blob(bytes) => format!("X'{}'", xenon_driver::hex(bytes)),
            Value::Null => "NULL".into(),
            Value::Integer(n) => n.to_string(),
            Value::Real(n) => n.to_string(),
            Value::Decimal(n) if xenon_driver::sql::is_plain_number(n) => n.clone(),
            Value::Decimal(n) => format!("'{}'", n.replace('\'', "''")),
        }
    }

    fn begin(&self) -> &'static str {
        "start transaction"
    }
}

/// A text-protocol value as Xenon shows it, guided by the column's type.
fn convert(value: mysql::Value, column: &mysql::Column) -> Value {
    let bytes = match value {
        mysql::Value::NULL => return Value::Null,
        mysql::Value::Bytes(bytes) => bytes,
        // The text protocol sends everything as bytes; these arrive only from
        // the binary protocol, but handle them rather than guess.
        mysql::Value::Int(v) => return Value::Integer(v),
        mysql::Value::UInt(v) => {
            return i64::try_from(v).map_or_else(|_| Value::Decimal(v.to_string()), Value::Integer);
        }
        mysql::Value::Float(v) => return Value::Decimal(v.to_string()),
        mysql::Value::Double(v) => return Value::Decimal(v.to_string()),
        other => return Value::Text(other.as_sql(true).trim_matches('\'').to_string()),
    };
    use ColumnType::*;
    match column.column_type() {
        MYSQL_TYPE_TINY | MYSQL_TYPE_SHORT | MYSQL_TYPE_INT24 | MYSQL_TYPE_LONG
        | MYSQL_TYPE_LONGLONG | MYSQL_TYPE_YEAR => {
            let text = String::from_utf8_lossy(&bytes);
            // Unsigned bigints can overflow i64; keep those exact as text.
            text.parse::<i64>()
                .map_or_else(|_| Value::Decimal(text.into_owned()), Value::Integer)
        }
        // Floats too, so they read as the server printed them (no `0.0000…1`).
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL | MYSQL_TYPE_FLOAT | MYSQL_TYPE_DOUBLE => {
            Value::Decimal(String::from_utf8_lossy(&bytes).into_owned())
        }
        MYSQL_TYPE_BIT | MYSQL_TYPE_GEOMETRY => Value::Blob(bytes),
        MYSQL_TYPE_TINY_BLOB
        | MYSQL_TYPE_MEDIUM_BLOB
        | MYSQL_TYPE_LONG_BLOB
        | MYSQL_TYPE_BLOB
        | MYSQL_TYPE_STRING
        | MYSQL_TYPE_VAR_STRING
        | MYSQL_TYPE_VARCHAR
            if column.character_set() == BINARY_CHARSET =>
        {
            Value::Blob(bytes)
        }
        _ => match String::from_utf8(bytes) {
            Ok(text) => Value::Text(text),
            Err(err) => Value::Blob(err.into_bytes()),
        },
    }
}

/// A short type name for a result column, like the ones `information_schema` uses.
fn type_label(column: &mysql::Column) -> String {
    use ColumnType::*;
    let binary = column.character_set() == BINARY_CHARSET;
    let name = match column.column_type() {
        MYSQL_TYPE_TINY => "tinyint",
        MYSQL_TYPE_SHORT => "smallint",
        MYSQL_TYPE_INT24 => "mediumint",
        MYSQL_TYPE_LONG => "int",
        MYSQL_TYPE_LONGLONG => "bigint",
        MYSQL_TYPE_YEAR => "year",
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => "decimal",
        MYSQL_TYPE_FLOAT => "float",
        MYSQL_TYPE_DOUBLE => "double",
        MYSQL_TYPE_BIT => "bit",
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => "date",
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => "time",
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => "datetime",
        MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => "timestamp",
        MYSQL_TYPE_JSON => "json",
        MYSQL_TYPE_ENUM => "enum",
        MYSQL_TYPE_SET => "set",
        MYSQL_TYPE_GEOMETRY => "geometry",
        MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB | MYSQL_TYPE_BLOB => {
            if binary {
                "blob"
            } else {
                "text"
            }
        }
        MYSQL_TYPE_STRING => {
            if column.flags().contains(ColumnFlags::ENUM_FLAG) {
                "enum"
            } else if column.flags().contains(ColumnFlags::SET_FLAG) {
                "set"
            } else if binary {
                "binary"
            } else {
                "char"
            }
        }
        MYSQL_TYPE_VAR_STRING | MYSQL_TYPE_VARCHAR => {
            if binary {
                "varbinary"
            } else {
                "varchar"
            }
        }
        MYSQL_TYPE_NULL => "null",
        _ => "unknown",
    };
    if column.flags().contains(ColumnFlags::UNSIGNED_FLAG) {
        format!("{name} unsigned")
    } else {
        name.to_string()
    }
}

fn describe_error(err: mysql::Error) -> anyhow::Error {
    match err {
        mysql::Error::MySqlError(err) => QueryError {
            message: err.message,
            position: None,
        }
        .into(),
        mysql::Error::IoError(err) => anyhow!("could not reach the server: {err}"),
        other => anyhow!("{other}"),
    }
}

/// MySQL reports syntax errors as "… near 'rest of statement' at line N"
/// rather than a position; find that text in `sql` to point at it.
fn locate(err: anyhow::Error, sql: &str) -> anyhow::Error {
    match err.downcast::<QueryError>() {
        Ok(mut err) => {
            if err.position.is_none() {
                err.position = near_position(&err.message, sql);
            }
            err.into()
        }
        Err(err) => err,
    }
}

fn near_position(message: &str, sql: &str) -> Option<usize> {
    let start = message.find("near '")? + "near '".len();
    let end = message.rfind("' at line ")?;
    let snippet = message.get(start..end)?;
    let line: usize = message[end + "' at line ".len()..].trim().parse().ok()?;
    let line_start = match line {
        0 | 1 => 0,
        n => sql
            .match_indices('\n')
            .nth(n - 2)
            .map_or(0, |(ix, _)| ix + 1),
    };
    // The server cuts the snippet at 80 characters; match on a prefix.
    let probe: String = snippet.chars().take(40).collect();
    if probe.is_empty() {
        return None;
    }
    // Line numbers count within the failing statement of a script, so fall
    // back to searching the whole text.
    let byte = sql[line_start..]
        .find(&probe)
        .map(|ix| ix + line_start)
        .or_else(|| sql.find(&probe))?;
    Some(sql[..byte].chars().count() + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_get_the_tls_they_ask_for() {
        let ssl = |url: &str| parse_url(url).map(|o| o.ssl);
        assert_eq!(
            ssl("mysql://h/db?ssl-ca=/ca.pem").unwrap(),
            SslMode::VerifyCa
        );
        assert_eq!(
            ssl("mysql://h/db?ssl-mode=required&ssl-ca=/ca.pem").unwrap(),
            SslMode::VerifyCa
        );
        assert_eq!(
            ssl("mysql://h/db?ssl-mode=verify_identity&ssl-ca=/ca.pem").unwrap(),
            SslMode::VerifyFull
        );
        assert_eq!(ssl("mysql://h/db?useSSL=true").unwrap(), SslMode::Require);
        assert_eq!(
            ssl("mysql://h/db?useSSL=true&verifyServerCertificate=true").unwrap(),
            SslMode::VerifyFull
        );
        assert_eq!(
            ssl("mysql://h/db?ssl_verify_server_cert=1").unwrap(),
            SslMode::VerifyFull
        );
        assert_eq!(
            ssl("mysql://h/db?charset=utf8mb4").unwrap(),
            SslMode::Prefer
        );
        assert!(ssl("mysql://h/db?ssl-capath=/certs").is_err());
        assert!(ssl("mysql://h/db?tls-version=TLSv1.3").is_err());
    }

    #[test]
    fn ssl_modes_are_read_strictly() {
        let mode = |url: &str| parse_url(url).map(|o| o.ssl);
        for (url, want) in [
            ("mysql://a@h/db?ssl-mode=verify_full", SslMode::VerifyFull),
            (
                "mysql://a@h/db?sslmode=VERIFY_IDENTITY",
                SslMode::VerifyFull,
            ),
            (
                "mysql://a@h/db?ssl-mode=VERIFY%5FIDENTITY",
                SslMode::VerifyFull,
            ),
            (
                "mysql://a@h/db?ssl-mode=VERIFY_IDENTITY&ssl-mode=DISABLED",
                SslMode::VerifyFull,
            ),
            ("mysql://a@h/db", SslMode::Prefer),
        ] {
            assert_eq!(mode(url).unwrap(), want, "{url}");
        }
        assert!(mode("mysql://a@h/db?ssl-mode=verify_fulll").is_err());
    }

    #[test]
    fn certificate_files_come_from_urls() {
        let options = parse_url(
            "mysql://me@db.example.com/shop?ssl-mode=VERIFY_IDENTITY&ssl-ca=%2Fcerts%2Fca.pem\
             &ssl_cert=/certs/me.pem&ssl-key=/certs/me.key",
        )
        .unwrap();
        assert_eq!(options.ssl, SslMode::VerifyFull);
        assert_eq!(options.tls.ca, "/certs/ca.pem");
        assert_eq!(options.tls.cert, "/certs/me.pem");
        assert_eq!(options.tls.key, "/certs/me.key");
    }

    #[test]
    fn urls() {
        let options =
            parse_url("mysql://app:s%40cret@db.example.com:3307/shop?ssl-mode=REQUIRED").unwrap();
        assert_eq!(options.host, "db.example.com");
        assert_eq!(options.port, Some(3307));
        assert_eq!(options.user, "app");
        assert_eq!(options.password, "s@cret");
        assert_eq!(options.database, "shop");
        assert_eq!(options.ssl, SslMode::Require);

        let minimal = parse_url("mariadb://localhost").unwrap();
        assert_eq!(minimal.host, "localhost");
        assert_eq!(minimal.port, None);
        assert_eq!(minimal.user, "");
        assert_eq!(minimal.ssl, SslMode::Prefer);

        assert!(parse_url("localhost:3306").is_err());
    }

    #[test]
    fn finds_syntax_error_positions() {
        let sql = "select 1;\nselect * fro orders where id = 1";
        let message = "You have an error in your SQL syntax; check the manual that corresponds to your \
                       MySQL server version for the right syntax to use near 'fro orders where id = 1' at line 2";
        let position = near_position(message, "select * fro orders where id = 1").unwrap();
        assert_eq!(position, 10);
        assert!(near_position(message, sql).is_some());
        assert_eq!(near_position("Table 'shop.nope' doesn't exist", sql), None);
    }
}

/// Run against the test fleet: `dev/fleet/up.sh mysql mariadb`, then
/// `cargo test -p xenon_mysql -- --ignored`.
#[cfg(test)]
mod live {
    use super::*;

    /// The live tests share one server and its catalog: run them one at a time.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    const DIALECT: xenon_driver::conformance::Dialect = xenon_driver::conformance::Dialect {
        integer: "int",
        text: "text",
        decimal: "decimal(12,2)",
        real: "double",
        blob: "blob",
        // Not a recursive CTE: MySQL and MariaDB both stop those at 1,000 rows by default.
        series: |n| {
            let digits = "(select 0 d union all select 1 union all select 2 union all select 3 \
                          union all select 4 union all select 5 union all select 6 \
                          union all select 7 union all select 8 union all select 9)";
            format!(
                "select a.d + 10 * b.d + 100 * c.d + 1000 * e.d + 10000 * f.d + 1 as i \
                 from {digits} a, {digits} b, {digits} c, {digits} e, {digits} f \
                 having i <= {n} order by i"
            )
        },
        // Not `sleep(5)`: a killed sleep returns 1 instead of failing.
        slow: "select benchmark(2000000000, sha2('xenon', 256))",
        rollback: "rollback",
        referential_actions: true,
        delete_returning: None,
    };

    /// Ending a session from another connection: `alive` notices.
    fn lost_connection_is_noticed(port: u16) {
        let (db, other) = (fleet(port), fleet(port));
        assert!(db.alive());
        let id = match db.execute("select connection_id()", 1).unwrap().outcome {
            QueryOutcome::Rows(rows) => rows.rows[0][0].clone(),
            other => panic!("expected rows, got {other:?}"),
        };
        other
            .execute(&format!("kill {}", db.literal(&id)), 1)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while db.alive() {
            assert!(
                Instant::now() < deadline,
                "still alive after its session was killed"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    #[ignore]
    fn lost_connection_mysql() {
        let _serial = serial();
        lost_connection_is_noticed(53306);
    }

    #[test]
    #[ignore]
    fn lost_connection_mariadb() {
        let _serial = serial();
        lost_connection_is_noticed(53307);
    }

    #[test]
    #[ignore]
    fn conformance_mysql() {
        let _serial = serial();
        xenon_driver::conformance::run(&fleet(53306), &DIALECT);
    }

    #[test]
    #[ignore]
    fn conformance_mariadb() {
        let _serial = serial();
        xenon_driver::conformance::run(
            &fleet(53307),
            &xenon_driver::conformance::Dialect {
                delete_returning: Some("delete from {table} returning id"),
                ..DIALECT
            },
        );
    }

    fn fleet_options(port: u16) -> ConnectOptions {
        ConnectOptions {
            host: "127.0.0.1".into(),
            port: Some(port),
            database: "shop".into(),
            user: "xenon".into(),
            password: "xenon".into(),
            ..Default::default()
        }
    }

    fn fleet(port: u16) -> MysqlConnection {
        MysqlConnection::connect_options(&fleet_options(port))
            .expect("is the fleet up? dev/fleet/up.sh mysql mariadb")
    }

    /// `xenon_cert` must present the fleet's client certificate (require x509);
    /// the fleet's CA makes verify-full pass.
    fn mutual_tls(port: u16) {
        let tls = |file: &str| format!("{}/../../dev/fleet/tls/{file}", env!("CARGO_MANIFEST_DIR"));
        let with = |cert: bool| ConnectOptions {
            user: "xenon_cert".into(),
            password: "xenon".into(),
            ssl: SslMode::VerifyFull,
            tls: TlsFiles {
                ca: tls("ca.pem"),
                cert: if cert {
                    tls("client.pem")
                } else {
                    String::new()
                },
                key: if cert {
                    tls("client.key")
                } else {
                    String::new()
                },
            },
            ..fleet_options(port)
        };
        let conn = MysqlConnection::connect_options(&with(true)).expect("client certificate login");
        let who = rows(&conn, "select current_user()");
        assert!(
            matches!(&who.rows[0][0], Value::Text(user) if user.starts_with("xenon_cert@")),
            "{:?}",
            who.rows[0][0]
        );
        assert!(
            MysqlConnection::connect_options(&with(false)).is_err(),
            "logged in without a client certificate"
        );
    }

    #[test]
    #[ignore]
    fn mutual_tls_mysql() {
        let _serial = serial();
        mutual_tls(53306);
    }

    #[test]
    #[ignore]
    fn mutual_tls_mariadb() {
        let _serial = serial();
        mutual_tls(53307);
    }

    fn rows(conn: &MysqlConnection, sql: &str) -> ResultSet {
        match conn.execute(sql, 100_000).unwrap().outcome {
            QueryOutcome::Rows(rows) => rows,
            other => panic!("expected rows, got {other:?}"),
        }
    }

    fn check(port: u16, mariadb: bool) {
        let conn = fleet(port);
        assert_eq!(conn.is_mariadb(), mariadb);
        // What tells it apart from another server at its address (W9).
        assert!(conn.server_identity().is_some());
        assert_eq!(conn.server_identity(), fleet(port).server_identity());
        assert!(
            conn.server_version()
                .starts_with(if mariadb { "mariadb " } else { "mysql " })
        );

        let schemas = conn.introspect().unwrap();
        assert_eq!(schemas[0].name, "shop");
        let tables: Vec<_> = schemas[0].objects.iter().map(|o| o.name.as_str()).collect();
        for table in [
            "customers",
            "orders",
            "paid_orders",
            "type_zoo",
            "Order Items (archive)",
        ] {
            assert!(tables.contains(&table), "{table} missing from {tables:?}");
        }
        let orders = schemas[0]
            .objects
            .iter()
            .find(|o| o.name == "orders")
            .unwrap();
        assert!(
            orders
                .columns
                .iter()
                .any(|c| c.name == "id" && c.primary_key)
        );

        // Exact values for every type, as the server prints them.
        let zoo = rows(&conn, "select * from type_zoo order by id");
        let col = |name: &str| zoo.columns.iter().position(|c| c.name == name).unwrap();
        let max = &zoo.rows[0];
        assert_eq!(max[col("big")], Value::Integer(i64::MAX));
        assert_eq!(
            max[col("ubig")],
            Value::Decimal("18446744073709551615".into())
        );
        assert_eq!(
            max[col("exact")],
            Value::Decimal("12345678901234567890.0123456789".into())
        );
        assert_eq!(max[col("born")], Value::Text("2024-02-29".into()));
        assert_eq!(
            max[col("at_dt")],
            Value::Text("2024-12-31 23:59:59.999999".into())
        );
        assert_eq!(max[col("perms")], Value::Text("read,admin".into()));
        assert_eq!(max[col("bytes")], Value::Blob(vec![0xde, 0xad, 0xbe, 0xef]));
        assert!(matches!(&max[col("doc")], Value::Text(doc) if doc.contains("🦀")));
        assert_eq!(
            zoo.columns[col("ubig")].type_name.as_deref(),
            Some("bigint unsigned")
        );
        assert!(zoo.rows[2].iter().skip(1).all(Value::is_null));

        // Odd identifiers, quoted.
        let odd = rows(
            &conn,
            &format!(
                "select * from {}",
                conn.quote_ident("Order Items (archive)")
            ),
        );
        assert_eq!(odd.rows.len(), 2);

        // Several statements: the last one's outcome.
        let result = conn.execute("select 1; select 2 as two", 10).unwrap();
        assert_eq!(result.statements, 2);
        let QueryOutcome::Rows(last) = result.outcome else {
            panic!()
        };
        assert_eq!(last.columns[0].name, "two");

        // Row limits cut the server off.
        let limited = conn.execute("select * from events", 100).unwrap();
        let QueryOutcome::Rows(limited) = limited.outcome else {
            panic!()
        };
        assert_eq!(limited.rows.len(), 100);
        assert!(limited.truncated);
        // ...and the connection is still usable afterwards.
        assert_eq!(
            rows(&conn, "select count(*) from orders").rows[0][0],
            Value::Integer(3000)
        );

        // Syntax errors point into the statement.
        let err = conn.execute("select * fro orders", 10).unwrap_err();
        let err = err.downcast_ref::<QueryError>().unwrap();
        assert_eq!(err.position, Some(10));

        // Cancel stops a long query.
        let cancel = conn.cancel_handle();
        let timer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            cancel.cancel();
        });
        let started = Instant::now();
        let slept = conn.execute("select sleep(10)", 10);
        timer.join().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "cancel didn't stop the query"
        );
        // MySQL returns 1 from an interrupted sleep; MariaDB may report an error.
        drop(slept);

        // Writes affect rows.
        let affected = conn
            .execute("update customers set city = city where id <= 3", 10)
            .unwrap()
            .outcome;
        assert!(matches!(affected, QueryOutcome::Affected(_)));

        // A later statement's error is reported, after a write or after rows,
        // not lost (a production script would be committed).
        for script in [
            "update customers set city = city where id = 1; insert into no_such_table values (1)",
            "select 1; delete from no_such_table",
        ] {
            assert!(conn.execute(script, 10).is_err(), "{script}");
        }

        // Text round-trips whatever the session's backslash mode, even one set
        // after connecting.
        for mode in ["NO_BACKSLASH_ESCAPES", ""] {
            conn.execute(&format!("set session sql_mode = '{mode}'"), 1)
                .unwrap();
            for text in [r"a\'b", r"C:\new\table", "it's", r"\"] {
                let back = match conn
                    .execute(
                        &format!("select {}", conn.literal(&Value::Text(text.into()))),
                        1,
                    )
                    .unwrap()
                    .outcome
                {
                    QueryOutcome::Rows(rows) => rows.rows[0][0].clone(),
                    other => panic!("{other:?}"),
                };
                assert_eq!(back, Value::Text(text.into()), "{text:?} under {mode:?}");
            }
        }
        conn.execute("set session sql_mode = default", 1).unwrap();

        // A change of case or trailing spaces elsewhere is noticed, despite the
        // default `_ci` / PAD SPACE collation.
        conn.execute(
            "drop table if exists xenon_case; create table xenon_case (id int primary key, v varchar(20)); \
             insert into xenon_case values (1, 'old')",
            1,
        )
        .unwrap();
        for (changed, loaded) in [("OLD", "old"), ("old  ", "old")] {
            conn.execute(&format!("update xenon_case set v = '{changed}'"), 1)
                .unwrap();
            let check = conn
                .unchanged("v", "varchar", &Value::Text(loaded.into()))
                .unwrap();
            let found = match conn
                .execute(&format!("select count(*) from xenon_case where {check}"), 1)
                .unwrap()
                .outcome
            {
                QueryOutcome::Rows(rows) => rows.rows[0][0].clone(),
                other => panic!("{other:?}"),
            };
            assert_eq!(found, Value::Integer(0), "{changed:?} looked unchanged");
            let same = conn
                .unchanged("v", "varchar", &Value::Text(changed.into()))
                .unwrap();
            let found = match conn
                .execute(&format!("select count(*) from xenon_case where {same}"), 1)
                .unwrap()
                .outcome
            {
                QueryOutcome::Rows(rows) => rows.rows[0][0].clone(),
                other => panic!("{other:?}"),
            };
            assert_eq!(found, Value::Integer(1), "{changed:?} didn't match itself");
            conn.execute("update xenon_case set v = 'old'", 1).unwrap();
        }
        conn.execute("drop table xenon_case", 1).unwrap();

        // An open transaction is known: after `begin`, ended by a schema change
        // (MySQL commits it), and with autocommit off.
        conn.execute("begin", 1).unwrap();
        assert_eq!(conn.in_transaction(), Some(true));
        conn.execute("create table xenon_implicit_commit (a int)", 1)
            .unwrap();
        assert_eq!(conn.in_transaction(), Some(false));
        conn.execute("drop table xenon_implicit_commit", 1).unwrap();
        conn.execute("set autocommit = 0", 1).unwrap();
        conn.execute("select * from customers limit 1", 1).unwrap();
        assert_eq!(conn.in_transaction(), Some(true));
        conn.execute("set autocommit = 1", 1).unwrap();
        assert_eq!(conn.in_transaction(), Some(false));
    }

    #[test]
    #[ignore]
    fn mysql() {
        let _serial = serial();
        check(53306, false);
    }

    #[test]
    #[ignore]
    fn mariadb() {
        let _serial = serial();
        check(53307, true);
    }

    /// Both images make a self-signed certificate on first start.
    #[test]
    #[ignore]
    fn tls_modes() {
        let _serial = serial();
        for port in [53306, 53307] {
            let with = |ssl| {
                MysqlConnection::connect_options(&ConnectOptions {
                    host: "127.0.0.1".into(),
                    port: Some(port),
                    user: "xenon".into(),
                    password: "xenon".into(),
                    ssl,
                    ..Default::default()
                })
            };
            for ssl in [SslMode::Disable, SslMode::Prefer, SslMode::Require] {
                assert!(with(ssl).is_ok(), "{ssl:?} on {port}");
            }
            assert!(
                with(SslMode::VerifyFull).is_err(),
                "verify-full accepted a self-signed cert on {port}"
            );
        }
    }

    #[test]
    #[ignore]
    fn read_only_refuses_writes_and_schema_changes() {
        let _serial = serial();
        for port in [53306, 53307] {
            let conn = MysqlConnection::connect_options(&ConnectOptions {
                host: "127.0.0.1".into(),
                port: Some(port),
                database: "shop".into(),
                user: "xenon".into(),
                password: "xenon".into(),
                read_only: true,
                ..Default::default()
            })
            .unwrap();
            for write in [
                "update customers set city = 'x' where id = 1",
                "drop table customers",
                "rename table customers to gone",
                "prepare s from 'drop table customers'",
                "call no_such_procedure()",
                "/*!50000 drop table customers */",
                "set session transaction read write",
                "set @@session.transaction_read_only = 0",
            ] {
                assert!(conn.execute(write, 10).is_err(), "{port}: {write}");
            }
            assert!(conn.execute("select count(*) from customers", 10).is_ok());
            // Still read-only after all that.
            assert!(
                conn.execute("update customers set city = 'x' where id = 1", 10)
                    .is_err()
            );
        }
    }

    #[test]
    #[ignore]
    fn lock_wait_gives_up() {
        let _serial = serial();
        for port in [53306, 53307] {
            let open = |lock_timeout| {
                MysqlConnection::connect_options(&ConnectOptions {
                    lock_timeout,
                    ..fleet_options(port)
                })
                .unwrap()
            };
            let holder = open(None);
            let waiter = open(Some(Duration::from_secs(1)));
            holder.execute("start transaction", 1).unwrap();
            holder
                .execute("update customers set city = city where id = 1", 1)
                .unwrap();
            let started = Instant::now();
            let blocked = waiter.execute("update customers set city = city where id = 1", 1);
            holder.execute("rollback", 1).unwrap();
            assert!(
                blocked.is_err(),
                "{port}: the second update didn't wait for the lock"
            );
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "{port}: waited too long"
            );
        }
    }
}
