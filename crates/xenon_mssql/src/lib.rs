//! SQL Server and Azure SQL driver, built on `tiberius` (TDS) over Tokio.
//!
//! SQL Server has no `LIMIT`, `EXPLAIN` or per-query cancel, so: browsing uses
//! `TOP`, Explain runs the statement under `SHOWPLAN_TEXT`, and cancelling sends
//! `KILL` from a second connection, after which the next query reconnects.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use futures_util::TryStreamExt as _;
use tiberius::{
    AuthMethod, Client, ColumnData, ColumnType, Config, EncryptionLevel, FromSql, QueryItem,
};
use tokio::net::TcpStream;
use tokio::runtime::Runtime;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt as _};
use xenon_driver::{
    CancelHandle, Column, ColumnInfo, Connection, Feed, ForeignKey, IndexInfo, ObjectColumn,
    ObjectKind, Progress, QueryError, QueryOutcome, QueryResult, ResultSet, Schema, SchemaObject,
    TableInfo, Value, referential_action,
};
pub use xenon_driver::{ConnectOptions, SslMode};

pub const DEFAULT_PORT: u16 = 1433;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Marks a statement for Explain; see [`MssqlConnection::execute`].
const SHOWPLAN: &str = "-- xenon:showplan\n";

type Tds = Client<Compat<TcpStream>>;

/// One shared Tokio runtime drives every SQL Server connection.
fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .thread_name("xenon-mssql")
            .build()
            .expect("tokio runtime")
    })
}

/// Fields from a `mssql://user:pass@host:1433/db` URL, a JDBC
/// `sqlserver://host:1433;databaseName=db;user=…` string, or an ADO.NET
/// `Server=tcp:host,1433;Database=db;User Id=…` string.
pub fn parse_url(text: &str) -> Result<ConnectOptions> {
    let text = text.trim();
    let lower = text.to_ascii_lowercase();
    let mut options = ConnectOptions::default();
    let mut encrypt: Option<String> = None;
    let mut trust = false;

    let pairs: Vec<(String, String)>;
    if let Some(rest) = lower
        .strip_prefix("mssql://")
        .map(|_| &text["mssql://".len()..])
    {
        // URL form: user:pass@host:port/database?encrypt=…
        let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
        let (authority, database) = rest.split_once('/').unwrap_or((rest, ""));
        let (credentials, host_port) = match authority.rsplit_once('@') {
            Some((credentials, host_port)) => (Some(credentials), host_port),
            None => (None, authority),
        };
        if let Some(credentials) = credentials {
            let (user, password) = credentials.split_once(':').unwrap_or((credentials, ""));
            options.user = decode(user);
            options.password = decode(password);
        }
        set_host_port(&mut options, host_port, ':')?;
        options.database = decode(database);
        pairs = query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .map(|(k, v)| (k.to_ascii_lowercase(), decode(v)))
            .collect();
    } else if let Some(start) = ["jdbc:sqlserver://", "sqlserver://"]
        .iter()
        .find(|prefix| lower.starts_with(*prefix))
        .map(|prefix| prefix.len())
    {
        // JDBC form: host[\instance][:port];key=value;…
        let rest = &text[start..];
        let (host_port, params) = rest.split_once(';').unwrap_or((rest, ""));
        set_host_port(&mut options, host_port, ':')?;
        pairs = key_values(params);
    } else if lower.contains("server=") || lower.contains("data source=") {
        pairs = key_values(text);
    } else {
        return Err(anyhow!("not a SQL Server connection string"));
    }

    for (key, value) in pairs {
        match key.as_str() {
            "server" | "data source" | "address" | "addr" => {
                let value = value.trim_start_matches("tcp:");
                set_host_port(&mut options, value, ',')?;
            }
            "database" | "databasename" | "initial catalog" => options.database = value,
            "user" | "username" | "user id" | "uid" => options.user = value,
            "password" | "pwd" => options.password = value,
            "encrypt" => encrypt = Some(value.to_ascii_lowercase()),
            "trustservercertificate" | "trust server certificate" => {
                trust = matches!(value.to_ascii_lowercase().as_str(), "true" | "yes")
            }
            _ => {}
        }
    }
    options.ssl = match encrypt.as_deref() {
        Some("false" | "no" | "optional") => SslMode::Prefer,
        Some("true" | "yes" | "mandatory" | "strict") if trust => SslMode::Require,
        Some("true" | "yes" | "mandatory" | "strict") => SslMode::VerifyFull,
        _ if trust => SslMode::Require,
        _ => SslMode::Prefer,
    };
    Ok(options)
}

fn key_values(text: &str) -> Vec<(String, String)> {
    text.split(';')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| {
            (
                k.trim().to_ascii_lowercase(),
                v.trim().trim_matches(['{', '}']).to_string(),
            )
        })
        .collect()
}

/// `host`, `host:1433` (URLs) or `host,1433` (ADO.NET). A named instance
/// (`host\SQLEXPRESS`) keeps its name in `host`.
fn set_host_port(options: &mut ConnectOptions, text: &str, separator: char) -> Result<()> {
    match text.rsplit_once(separator) {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            options.host = host.to_string();
            options.port = Some(port.parse()?);
        }
        _ => options.host = text.to_string(),
    }
    Ok(())
}

fn decode(part: &str) -> String {
    let bytes = part.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = part
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
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

pub struct MssqlConnection {
    name: String,
    id: String,
    local: bool,
    version: String,
    schema: Option<String>,
    options: ConnectOptions,
    /// `None` after a cancel killed the session; the next query reconnects.
    client: Mutex<Option<Tds>>,
    cancel: Arc<MsCancel>,
}

struct MsCancel {
    options: ConnectOptions,
    /// The session id (`@@SPID`) of the current connection.
    spid: Mutex<i32>,
    /// Set when a cancel kills the session, so the next query reconnects.
    killed: std::sync::atomic::AtomicBool,
}

impl CancelHandle for MsCancel {
    fn cancel(&self) {
        let options = self.options.clone();
        let spid = *self.spid.lock().unwrap_or_else(|e| e.into_inner());
        self.killed.store(true, std::sync::atomic::Ordering::SeqCst);
        std::thread::spawn(move || {
            // SQL Server can't stop just the query, so end the session.
            let _ = runtime().block_on(async move {
                let mut killer = open(&options).await?;
                killer
                    .simple_query(format!("KILL {spid}"))
                    .await?
                    .into_results()
                    .await?;
                anyhow::Ok(())
            });
        });
    }
}

async fn open(options: &ConnectOptions) -> Result<Tds> {
    let host = if options.host.is_empty() {
        "localhost"
    } else {
        &options.host
    };
    let port = options.port.unwrap_or(DEFAULT_PORT);
    let mut config = Config::new();
    // A named instance (`host\SQLEXPRESS`) needs its port given explicitly here:
    // Xenon doesn't ask the SQL Browser service for it.
    let (server, instance) = host.split_once('\\').unwrap_or((host, ""));
    config.host(server);
    if !instance.is_empty() {
        config.instance_name(instance);
    }
    config.port(port);
    config.application_name("Xenon");
    if !options.database.is_empty() {
        config.database(&options.database);
    }
    let user = if options.user.is_empty() {
        "sa"
    } else {
        &options.user
    };
    config.authentication(AuthMethod::sql_server(user, &options.password));
    match options.ssl {
        SslMode::Disable => config.encryption(EncryptionLevel::NotSupported),
        // SQL Server's own default: encrypt the login, trust the certificate.
        SslMode::Prefer => {
            config.encryption(EncryptionLevel::Off);
            config.trust_cert();
        }
        SslMode::Require => {
            config.encryption(EncryptionLevel::Required);
            config.trust_cert();
        }
        // native-tls can't check the chain without the name; both verify fully.
        SslMode::VerifyCa | SslMode::VerifyFull => config.encryption(EncryptionLevel::Required),
    }
    // Through a tunnel: connect to its local end, verify the certificate for `host`.
    let address = match options.address {
        Some(address) => address,
        None => tokio::net::lookup_host((server, port))
            .await?
            .next()
            .ok_or_else(|| anyhow!("Couldn't find {server}."))?,
    };
    let tcp = connect_tcp(address).await?;
    let mut client = match Client::connect(config.clone(), tcp.compat_write()).await {
        Ok(client) => client,
        // Azure SQL sends clients on to the node that holds the database.
        Err(tiberius::error::Error::Routing { host, port }) => {
            config.host(&host);
            config.port(port);
            let address = tokio::net::lookup_host((host.as_str(), port))
                .await?
                .next()
                .ok_or_else(|| anyhow!("Couldn't find {host}."))?;
            let tcp = connect_tcp(address).await?;
            Client::connect(config, tcp.compat_write())
                .await
                .map_err(describe)?
        }
        Err(err) => return Err(describe(err)),
    };
    if let Some(wait) = options.lock_timeout {
        client
            .simple_query(format!("set lock_timeout {}", wait.as_millis().max(1)))
            .await
            .map_err(describe)?
            .into_results()
            .await
            .map_err(describe)?;
    }
    Ok(client)
}

async fn connect_tcp(address: SocketAddr) -> Result<TcpStream> {
    let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address))
        .await
        .map_err(|_| anyhow!("Timed out connecting to {address}."))?
        .map_err(|err| anyhow!("could not reach the server: {err}"))?;
    tcp.set_nodelay(true)?;
    Ok(tcp)
}

impl MssqlConnection {
    pub fn connect_options(options: &ConnectOptions) -> Result<Self> {
        let host = if options.host.is_empty() {
            "localhost".to_string()
        } else {
            options.host.clone()
        };
        let port = options.port.unwrap_or(DEFAULT_PORT);
        let user = if options.user.is_empty() {
            "sa"
        } else {
            &options.user
        };
        let (client, spid, version, schema) = runtime().block_on(async {
            let mut client = open(options).await?;
            let row = client
                .simple_query(
                    "select @@spid, cast(serverproperty('ProductVersion') as nvarchar(64)), \
                     cast(serverproperty('EngineEdition') as int), schema_name()",
                )
                .await
                .map_err(describe)?
                .into_row()
                .await
                .map_err(describe)?
                .ok_or_else(|| anyhow!("no reply from the server"))?;
            let spid = row.get::<i16, _>(0).map(i32::from).unwrap_or_default();
            let version: String = row.get::<&str, _>(1).unwrap_or_default().to_string();
            let azure = row.get::<i32, _>(2) == Some(5);
            let schema = row.get::<&str, _>(3).map(str::to_string);
            let label = if azure { "azure sql" } else { "sqlserver" };
            anyhow::Ok((client, spid, format!("{label} {version}"), schema))
        })?;
        let database = &options.database;
        Ok(Self {
            name: if database.is_empty() {
                format!("{user}@{host}")
            } else {
                format!("{database}@{host}")
            },
            id: format!("mssql:{user}@{host}:{port}/{database}"),
            local: matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1"),
            version,
            schema,
            cancel: Arc::new(MsCancel {
                options: options.clone(),
                spid: Mutex::new(spid),
                killed: Default::default(),
            }),
            options: options.clone(),
            client: Mutex::new(Some(client)),
        })
    }

    /// Returns the outcome, statements run, whether the session must be
    /// replaced (rows were left unread), and whether a cancel stopped it.
    async fn run(
        client: &mut Tds,
        sql: &str,
        row_limit: usize,
        feed: &mut Feed<'_>,
        killed: &std::sync::atomic::AtomicBool,
    ) -> Result<(QueryOutcome, usize, bool, bool)> {
        let mut stream = client.simple_query(sql).await.map_err(describe)?;
        let mut sets: Vec<ResultSet> = Vec::new();
        let mut types: Vec<ColumnType> = Vec::new();
        let mut over_limit = false;
        let writes = xenon_driver::sql::writes(sql);
        let mut cancelled = false;
        loop {
            let item = match stream.try_next().await {
                Ok(Some(item)) => item,
                Ok(None) => break,
                // Killed by a cancel while reading rows: keep what arrived.
                Err(_)
                    if killed.load(std::sync::atomic::Ordering::SeqCst)
                        && sets.last().is_some_and(|s| !s.rows.is_empty()) =>
                {
                    if let Some(set) = sets.last_mut() {
                        set.truncated = true;
                    }
                    cancelled = true;
                    break;
                }
                Err(err) => return Err(describe(err)),
            };
            match item {
                QueryItem::Metadata(meta) => {
                    feed.next_set();
                    types = meta.columns().iter().map(|c| c.column_type()).collect();
                    sets.push(ResultSet {
                        columns: meta
                            .columns()
                            .iter()
                            .map(|c| Column {
                                name: c.name().to_string(),
                                type_name: Some(type_label(c.column_type()).to_string()),
                            })
                            .collect(),
                        ..Default::default()
                    });
                }
                QueryItem::Row(row) => {
                    let Some(set) = sets.last_mut() else { continue };
                    if set.rows.len() == row_limit {
                        set.truncated = true;
                        // A write must finish (ending the session would roll it
                        // back), so its extra rows are read and dropped instead.
                        if writes {
                            continue;
                        }
                        over_limit = true;
                        break;
                    }
                    set.rows.push(
                        row.into_iter()
                            .zip(&types)
                            .map(|(data, ty)| convert(data, *ty))
                            .collect(),
                    );
                    feed.read(set, |_| {});
                }
            }
        }
        let statements = sets.len().max(1);
        if cancelled {
            let rows = sets.pop().expect("checked above");
            return Ok((QueryOutcome::Rows(rows), statements, true, true));
        }
        let outcome = match sets.pop() {
            Some(rows) => QueryOutcome::Rows(rows),
            None => {
                drop(stream);
                // No result set: report how many rows the last statement changed.
                let count = client
                    .simple_query("select @@rowcount")
                    .await
                    .map_err(describe)?
                    .into_row()
                    .await
                    .map_err(describe)?
                    .and_then(|row| row.get::<i32, _>(0))
                    .unwrap_or(0);
                QueryOutcome::Affected(count.max(0) as u64)
            }
        };
        Ok((outcome, statements, over_limit, false))
    }
}

impl Connection for MssqlConnection {
    fn name(&self) -> &str {
        &self.name
    }

    fn id(&self) -> String {
        self.id.clone()
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
        // SQL Server has no read-only session, so refuse what would write.
        let statement = sql.strip_prefix(SHOWPLAN).unwrap_or(sql);
        if self.options.read_only
            && let Some((_, what)) = xenon_driver::sql::assess(statement)
        {
            return Err(QueryError {
                message: format!("This is a read-only connection, so {what} isn't allowed."),
                position: None,
            }
            .into());
        }
        let started = Instant::now();
        let mut feed = Feed::new(progress);
        let mut guard = self.client.lock().unwrap_or_else(|e| e.into_inner());
        let killed = || {
            self.cancel
                .killed
                .swap(false, std::sync::atomic::Ordering::SeqCst)
        };
        if killed() {
            *guard = None;
        }
        if guard.is_none() {
            // The last session was killed (cancel or row limit): open another.
            let fresh = runtime().block_on(async {
                let mut client = open(&self.options).await?;
                let spid = client
                    .simple_query("select @@spid")
                    .await
                    .map_err(describe)?
                    .into_row()
                    .await
                    .map_err(describe)?
                    .and_then(|row| row.get::<i16, _>(0))
                    .map(i32::from)
                    .unwrap_or_default();
                anyhow::Ok((client, spid))
            })?;
            *self.cancel.spid.lock().unwrap_or_else(|e| e.into_inner()) = fresh.1;
            *guard = Some(fresh.0);
        }
        let client = guard.as_mut().expect("connected above");

        let result = runtime().block_on(async {
            if let Some(statement) = sql.strip_prefix(SHOWPLAN) {
                // SHOWPLAN_TEXT must be alone in its batch, so three batches.
                client
                    .simple_query("set showplan_text on")
                    .await
                    .map_err(describe)?
                    .into_results()
                    .await
                    .map_err(describe)?;
                let plan =
                    Self::run(client, statement, row_limit, &mut feed, &self.cancel.killed).await;
                client
                    .simple_query("set showplan_text off")
                    .await
                    .map_err(describe)?
                    .into_results()
                    .await
                    .map_err(describe)?;
                plan
            } else {
                Self::run(client, sql, row_limit, &mut feed, &self.cancel.killed).await
            }
        });
        match result {
            Ok((outcome, statements, over_limit, cancelled)) => {
                if over_limit {
                    // Unread rows would block the connection (or a cancel ended
                    // the session): start a new one next time.
                    *guard = None;
                }
                if cancelled {
                    killed();
                }
                Ok(QueryResult {
                    outcome,
                    statements,
                    elapsed: started.elapsed(),
                    cancelled,
                })
            }
            Err(_) if killed() => {
                *guard = None;
                Err(anyhow!(
                    "Cancelled. (SQL Server ends the session to stop a query; the next one reconnects.)"
                ))
            }
            Err(err) if !err.is::<QueryError>() => {
                // The connection broke: reconnect next time.
                *guard = None;
                Err(err)
            }
            Err(err) => Err(locate(err, sql)),
        }
    }

    fn introspect(&self) -> Result<Vec<Schema>> {
        const QUERY: &str = "
            select c.TABLE_SCHEMA, c.TABLE_NAME, t.TABLE_TYPE, c.COLUMN_NAME,
                   c.DATA_TYPE + case
                       when c.CHARACTER_MAXIMUM_LENGTH = -1 then '(max)'
                       when c.CHARACTER_MAXIMUM_LENGTH is not null then '(' + cast(c.CHARACTER_MAXIMUM_LENGTH as varchar) + ')'
                       when c.DATA_TYPE in ('decimal', 'numeric') then '(' + cast(c.NUMERIC_PRECISION as varchar) + ',' + cast(c.NUMERIC_SCALE as varchar) + ')'
                       else '' end,
                   case when c.IS_NULLABLE = 'NO' then 1 else 0 end,
                   case when k.COLUMN_NAME is null then 0 else 1 end
            from INFORMATION_SCHEMA.COLUMNS c
            join INFORMATION_SCHEMA.TABLES t on t.TABLE_SCHEMA = c.TABLE_SCHEMA and t.TABLE_NAME = c.TABLE_NAME
            left join (
                select ku.TABLE_SCHEMA, ku.TABLE_NAME, ku.COLUMN_NAME
                from INFORMATION_SCHEMA.TABLE_CONSTRAINTS tc
                join INFORMATION_SCHEMA.KEY_COLUMN_USAGE ku
                  on ku.CONSTRAINT_NAME = tc.CONSTRAINT_NAME and ku.TABLE_SCHEMA = tc.TABLE_SCHEMA
                where tc.CONSTRAINT_TYPE = 'PRIMARY KEY'
            ) k on k.TABLE_SCHEMA = c.TABLE_SCHEMA and k.TABLE_NAME = c.TABLE_NAME and k.COLUMN_NAME = c.COLUMN_NAME
            order by case when c.TABLE_SCHEMA = schema_name() then 0 else 1 end, c.TABLE_SCHEMA, c.TABLE_NAME, c.ORDINAL_POSITION";
        let rows = match self.execute(QUERY, usize::MAX)?.outcome {
            QueryOutcome::Rows(rows) => rows.rows,
            QueryOutcome::Affected(_) => Vec::new(),
        };
        let text = |v: &Value| match v {
            Value::Text(t) => t.clone(),
            other => other.display().into_owned(),
        };
        let mut schemas: Vec<Schema> = Vec::new();
        for row in rows {
            let (schema, table) = (text(&row[0]), text(&row[1]));
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
                    kind: if text(&row[2]) == "VIEW" {
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
                    name: text(&row[3]),
                    type_name: text(&row[4]),
                    not_null: row[5] == Value::Integer(1),
                    primary_key: row[6] == Value::Integer(1),
                });
        }
        Ok(schemas)
    }

    fn describe(&self, schema: &str, table: &str) -> Result<TableInfo> {
        let object = self.literal(&Value::Text(format!(
            "{}.{}",
            self.quote_ident(schema),
            self.quote_ident(table)
        )));
        let (schema_lit, table_lit) = (
            self.literal(&Value::Text(schema.into())),
            self.literal(&Value::Text(table.into())),
        );
        let rows = |sql: String| -> Result<Vec<Vec<Value>>> {
            Ok(match self.execute(&sql, usize::MAX)?.outcome {
                QueryOutcome::Rows(rows) => rows.rows,
                QueryOutcome::Affected(_) => Vec::new(),
            })
        };
        let text = |v: &Value| match v {
            Value::Null => None,
            Value::Text(t) => Some(t.clone()),
            other => Some(other.display().into_owned()),
        };
        let string = |v: &Value| text(v).unwrap_or_default();
        let yes = |v: &Value| *v == Value::Integer(1);

        let mut info = TableInfo::default();
        let columns = rows(format!(
            "select c.COLUMN_NAME,
                    c.DATA_TYPE + case
                        when c.CHARACTER_MAXIMUM_LENGTH = -1 then '(max)'
                        when c.CHARACTER_MAXIMUM_LENGTH is not null then '(' + cast(c.CHARACTER_MAXIMUM_LENGTH as varchar) + ')'
                        when c.DATA_TYPE in ('decimal', 'numeric') then '(' + cast(c.NUMERIC_PRECISION as varchar) + ',' + cast(c.NUMERIC_SCALE as varchar) + ')'
                        else '' end,
                    case when c.IS_NULLABLE = 'YES' then 1 else 0 end,
                    case when columnproperty(object_id({object}), c.COLUMN_NAME, 'IsIdentity') = 1
                         then 'identity' else c.COLUMN_DEFAULT end,
                    case when exists (
                        select 1 from sys.indexes i
                        join sys.index_columns ic on ic.object_id = i.object_id and ic.index_id = i.index_id
                        where i.object_id = object_id({object}) and i.is_primary_key = 1
                          and col_name(ic.object_id, ic.column_id) = c.COLUMN_NAME) then 1 else 0 end,
                    (select cast(ep.value as nvarchar(max)) from sys.extended_properties ep
                     where ep.major_id = object_id({object}) and ep.name = 'MS_Description'
                       and ep.minor_id = columnproperty(object_id({object}), c.COLUMN_NAME, 'ColumnId'))
             from INFORMATION_SCHEMA.COLUMNS c
             where c.TABLE_SCHEMA = {schema_lit} and c.TABLE_NAME = {table_lit}
             order by c.ORDINAL_POSITION"
        ))?;
        for row in columns {
            info.columns.push(ColumnInfo {
                name: string(&row[0]),
                type_name: string(&row[1]),
                nullable: yes(&row[2]),
                default: text(&row[3]).map(|d| unwrap_parens(&d).to_string()),
                primary_key: yes(&row[4]),
                comment: text(&row[5]),
            });
        }
        let indexes = rows(format!(
            "select i.name, cast(i.is_unique as int), cast(i.is_primary_key as int),
                    col_name(ic.object_id, ic.column_id)
             from sys.indexes i
             join sys.index_columns ic on ic.object_id = i.object_id and ic.index_id = i.index_id
             where i.object_id = object_id({object}) and i.type > 0 and ic.is_included_column = 0
             order by i.is_primary_key desc, i.name, ic.key_ordinal"
        ))?;
        for row in indexes {
            let name = string(&row[0]);
            match info.indexes.last_mut() {
                Some(last) if last.name == name => last.columns.push(string(&row[3])),
                _ => info.indexes.push(IndexInfo {
                    name,
                    unique: yes(&row[1]),
                    primary: yes(&row[2]),
                    columns: vec![string(&row[3])],
                }),
            }
        }
        let keys = rows(format!(
            "select fk.name,
                    object_schema_name(fk.parent_object_id), object_name(fk.parent_object_id),
                    col_name(fkc.parent_object_id, fkc.parent_column_id),
                    object_schema_name(fk.referenced_object_id), object_name(fk.referenced_object_id),
                    col_name(fkc.referenced_object_id, fkc.referenced_column_id),
                    fk.delete_referential_action_desc, fk.update_referential_action_desc
             from sys.foreign_keys fk
             join sys.foreign_key_columns fkc on fkc.constraint_object_id = fk.object_id
             where fk.parent_object_id = object_id({object})
                or fk.referenced_object_id = object_id({object})
             order by object_schema_name(fk.parent_object_id), object_name(fk.parent_object_id),
                      fk.name, fkc.constraint_column_id"
        ))?;
        let mut all: Vec<ForeignKey> = Vec::new();
        for row in keys {
            let (name, from_schema, from_table) =
                (string(&row[0]), string(&row[1]), string(&row[2]));
            match all.last_mut() {
                Some(last)
                    if last.name == name
                        && last.schema == from_schema
                        && last.table == from_table =>
                {
                    last.columns.push(string(&row[3]));
                    last.ref_columns.push(string(&row[6]));
                }
                _ => all.push(ForeignKey {
                    name,
                    schema: from_schema,
                    table: from_table,
                    columns: vec![string(&row[3])],
                    ref_schema: string(&row[4]),
                    ref_table: string(&row[5]),
                    ref_columns: vec![string(&row[6])],
                    on_delete: referential_action(&string(&row[7])),
                    on_update: referential_action(&string(&row[8])),
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
        // Views, procedures and the like keep their source; tables don't.
        info.definition = rows(format!("select object_definition(object_id({object}))"))?
            .first()
            .and_then(|row| text(&row[0]));
        Ok(info)
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        self.cancel.clone()
    }

    fn in_transaction(&self) -> Option<bool> {
        let mut guard = self.client.lock().unwrap_or_else(|e| e.into_inner());
        let client = guard.as_mut()?;
        runtime()
            .block_on(async {
                client
                    .simple_query("select @@trancount")
                    .await
                    .ok()?
                    .into_row()
                    .await
                    .ok()?
                    .and_then(|row| row.get::<i32, _>(0))
            })
            .map(|count| count > 0)
    }

    fn is_local(&self) -> bool {
        self.local
    }

    fn explain(&self, sql: &str) -> String {
        format!("{SHOWPLAN}{sql}")
    }

    fn quote_ident(&self, ident: &str) -> String {
        format!("[{}]", ident.replace(']', "]]"))
    }

    fn default_schema(&self) -> Option<String> {
        self.schema.clone()
    }

    fn uses_top(&self) -> bool {
        true
    }

    fn literal(&self, value: &Value) -> String {
        match value {
            // N'…' keeps non-Latin text intact in nvarchar columns.
            Value::Text(text) => format!("N'{}'", text.replace('\'', "''")),
            Value::Blob(bytes) => format!("0x{}", xenon_driver::hex(bytes)),
            Value::Null => "NULL".into(),
            Value::Integer(n) => n.to_string(),
            Value::Real(n) => n.to_string(),
            Value::Decimal(n) => n.clone(),
        }
    }

    fn begin(&self) -> &'static str {
        "begin transaction"
    }
}

/// A value as Xenon shows it: numbers exact, dates as SQL Server prints them.
fn convert(data: ColumnData<'static>, ty: ColumnType) -> Value {
    fn date<T: for<'a> FromSql<'a> + ToString>(data: &ColumnData<'static>) -> Value {
        match T::from_sql(data) {
            Ok(Some(value)) => Value::Text(value.to_string()),
            _ => Value::Null,
        }
    }
    match data {
        ColumnData::U8(v) => v.map_or(Value::Null, |v| Value::Integer(v.into())),
        ColumnData::I16(v) => v.map_or(Value::Null, |v| Value::Integer(v.into())),
        ColumnData::I32(v) => v.map_or(Value::Null, |v| Value::Integer(v.into())),
        ColumnData::I64(v) => v.map_or(Value::Null, Value::Integer),
        ColumnData::F32(v) => v.map_or(Value::Null, |v| Value::Decimal(v.to_string())),
        // money arrives as a float (tiberius decodes it so); it has four decimal
        // places, exact up to about 900 billion.
        ColumnData::F64(v) if matches!(ty, ColumnType::Money | ColumnType::Money4) => {
            v.map_or(Value::Null, |v| Value::Decimal(format!("{v:.4}")))
        }
        ColumnData::F64(v) => v.map_or(Value::Null, |v| Value::Decimal(v.to_string())),
        ColumnData::Bit(v) => v.map_or(Value::Null, |v| Value::Integer(v.into())),
        ColumnData::String(v) => v.map_or(Value::Null, |v| Value::Text(v.into_owned())),
        ColumnData::Guid(v) => v.map_or(Value::Null, |v| Value::Text(v.to_string().to_uppercase())),
        ColumnData::Binary(v) => v.map_or(Value::Null, |v| Value::Blob(v.into_owned())),
        ColumnData::Numeric(v) => v.map_or(Value::Null, |v| Value::Decimal(v.to_string())),
        ColumnData::Xml(v) => v.map_or(Value::Null, |v| Value::Text(v.to_string())),
        data @ (ColumnData::DateTime(_)
        | ColumnData::SmallDateTime(_)
        | ColumnData::DateTime2(_)) => date::<chrono::NaiveDateTime>(&data),
        data @ ColumnData::Date(_) => date::<chrono::NaiveDate>(&data),
        data @ ColumnData::Time(_) => date::<chrono::NaiveTime>(&data),
        data @ ColumnData::DateTimeOffset(_) => {
            date::<chrono::DateTime<chrono::FixedOffset>>(&data)
        }
    }
}

fn type_label(ty: ColumnType) -> &'static str {
    match ty {
        ColumnType::Null => "null",
        ColumnType::Bit | ColumnType::Bitn => "bit",
        ColumnType::Int1 => "tinyint",
        ColumnType::Int2 => "smallint",
        ColumnType::Int4 | ColumnType::Intn => "int",
        ColumnType::Int8 => "bigint",
        ColumnType::Float4 => "real",
        ColumnType::Float8 | ColumnType::Floatn => "float",
        ColumnType::Money | ColumnType::Money4 => "money",
        ColumnType::Datetime | ColumnType::Datetimen => "datetime",
        ColumnType::Datetime4 => "smalldatetime",
        ColumnType::Datetime2 => "datetime2",
        ColumnType::DatetimeOffsetn => "datetimeoffset",
        ColumnType::Daten => "date",
        ColumnType::Timen => "time",
        ColumnType::Guid => "uniqueidentifier",
        ColumnType::Decimaln => "decimal",
        ColumnType::Numericn => "numeric",
        ColumnType::BigVarBin | ColumnType::BigBinary | ColumnType::Image => "varbinary",
        ColumnType::BigVarChar | ColumnType::BigChar | ColumnType::Text => "varchar",
        ColumnType::NVarchar | ColumnType::NChar | ColumnType::NText => "nvarchar",
        ColumnType::Xml => "xml",
        ColumnType::Udt => "udt",
        ColumnType::SSVariant => "sql_variant",
    }
}

fn describe(err: tiberius::error::Error) -> anyhow::Error {
    match err {
        tiberius::error::Error::Server(token) => QueryError {
            message: token.message().to_string(),
            // Only the line is reported; `locate` turns it into a position.
            position: Some(token.line() as usize),
        }
        .into(),
        tiberius::error::Error::Io { message, .. } => {
            anyhow!("could not reach the server: {message}")
        }
        other => anyhow!("{other}"),
    }
}

/// SQL Server reports the line an error is on; point at the start of it.
fn locate(err: anyhow::Error, sql: &str) -> anyhow::Error {
    match err.downcast::<QueryError>() {
        Ok(mut err) => {
            err.position = err.position.and_then(|line| {
                let start = match line {
                    0 | 1 => 0,
                    n => sql.match_indices('\n').nth(n - 2).map(|(ix, _)| ix + 1)?,
                };
                // Skip indentation so the cursor lands on the statement.
                let indent = sql[start..]
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .count();
                Some(sql[..start].chars().count() + indent + 1)
            });
            err.into()
        }
        Err(err) => err,
    }
}

/// SQL Server prints defaults wrapped in parentheses: `((0))`, `(getdate())`.
fn unwrap_parens(text: &str) -> &str {
    let mut text = text.trim();
    while let Some(inner) = text.strip_prefix('(').and_then(|t| t.strip_suffix(')')) {
        // Only when the outer pair encloses everything: not `(a) + (b)`.
        let mut depth = 0;
        let balanced = inner.chars().all(|c| {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            depth >= 0
        });
        if !balanced {
            break;
        }
        text = inner.trim();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_strings() {
        let url = parse_url("mssql://sa:p%40ss@db.example.com:1444/shop?encrypt=true").unwrap();
        assert_eq!(
            (url.user.as_str(), url.password.as_str(), url.host.as_str()),
            ("sa", "p@ss", "db.example.com")
        );
        assert_eq!(
            (url.port, url.database.as_str(), url.ssl),
            (Some(1444), "shop", SslMode::VerifyFull)
        );

        let jdbc = parse_url("jdbc:sqlserver://db:1433;databaseName=shop;user=app;password=pw;trustServerCertificate=true").unwrap();
        assert_eq!(
            (jdbc.host.as_str(), jdbc.port, jdbc.database.as_str()),
            ("db", Some(1433), "shop")
        );
        assert_eq!(
            (jdbc.user.as_str(), jdbc.password.as_str(), jdbc.ssl),
            ("app", "pw", SslMode::Require)
        );

        let ado = parse_url("Server=tcp:acme.database.windows.net,1433;Initial Catalog=prod;User ID=me;Password={p;w};Encrypt=True;").unwrap();
        assert_eq!(
            (ado.host.as_str(), ado.port, ado.database.as_str()),
            ("acme.database.windows.net", Some(1433), "prod")
        );
        assert_eq!(ado.ssl, SslMode::VerifyFull);

        let instance = parse_url("Data Source=laptop\\SQLEXPRESS;Database=dev").unwrap();
        assert_eq!(instance.host, "laptop\\SQLEXPRESS");
        assert!(parse_url("postgres://x").is_err());
    }

    #[test]
    fn error_lines_become_positions() {
        let sql = "select 1;\n  selec * from orders";
        let err = locate(
            QueryError {
                message: "x".into(),
                position: Some(2),
            }
            .into(),
            sql,
        );
        assert_eq!(err.downcast_ref::<QueryError>().unwrap().position, Some(13));
    }
}

/// Against the test fleet: `dev/fleet/up.sh mssql`, then `cargo test -p xenon_mssql -- --ignored`.
#[cfg(test)]
mod live {
    use super::*;

    #[test]
    #[ignore]
    fn read_only_refuses_writes() {
        let conn = MssqlConnection::connect_options(&ConnectOptions {
            read_only: true,
            ..fleet().options.clone()
        })
        .unwrap();
        assert!(conn.execute("select 1", 10).is_ok());
        for write in [
            "delete from customers where id = 1",
            "exec sp_rename 'customers', 'gone'",
            "select * into xenon_ro_copy from customers",
            "drop table customers",
        ] {
            let err = conn.execute(write, 10).unwrap_err().to_string();
            assert!(err.contains("read-only connection"), "{write}: {err}");
        }
    }

    #[test]
    #[ignore]
    fn conformance() {
        xenon_driver::conformance::run(
            &fleet(),
            &xenon_driver::conformance::Dialect {
                integer: "int",
                text: "nvarchar(max)",
                decimal: "decimal(12,2)",
                real: "float",
                blob: "varbinary(max)",
                series: |n| {
                    format!(
                        "select top ({n}) row_number() over (order by (select null)) as i \
                         from sys.all_objects a cross join sys.all_objects b"
                    )
                },
                slow: "waitfor delay '00:00:05'",
                rollback: "rollback",
                referential_actions: true,
                delete_returning: Some("delete from {table} output deleted.id"),
            },
        );
    }

    fn fleet() -> MssqlConnection {
        MssqlConnection::connect_options(&ConnectOptions {
            host: "127.0.0.1".into(),
            port: Some(51433),
            database: "shop".into(),
            user: "sa".into(),
            password: "Xenon-fleet-1".into(),
            ..Default::default()
        })
        .expect("is the fleet up? dev/fleet/up.sh mssql")
    }

    fn rows(conn: &MssqlConnection, sql: &str) -> ResultSet {
        match conn.execute(sql, 100_000).unwrap().outcome {
            QueryOutcome::Rows(rows) => rows,
            other => panic!("expected rows, got {other:?}"),
        }
    }

    #[test]
    #[ignore]
    fn everything_against_the_fleet() {
        let conn = fleet();
        assert!(
            conn.server_version().starts_with("sqlserver 16."),
            "{}",
            conn.server_version()
        );
        assert_eq!(conn.default_schema().as_deref(), Some("dbo"));

        let schemas = conn.introspect().unwrap();
        assert_eq!(schemas[0].name, "dbo");
        let names: Vec<_> = schemas[0].objects.iter().map(|o| o.name.as_str()).collect();
        for table in [
            "customers",
            "orders",
            "paid_orders",
            "type_zoo",
            "Order Items (archive)",
        ] {
            assert!(names.contains(&table), "{table} missing from {names:?}");
        }
        assert!(schemas.iter().any(|s| s.name == "analytics"));

        let zoo = rows(&conn, "select * from type_zoo order by id");
        let col = |name: &str| zoo.columns.iter().position(|c| c.name == name).unwrap();
        let max = &zoo.rows[0];
        assert_eq!(max[col("big")], Value::Integer(i64::MAX));
        assert_eq!(
            max[col("exact")],
            Value::Decimal("1234567890123456789012345678.0123456789".into())
        );
        // money arrives as an f64 from tiberius, so only amounts below 2^53 / 10^4
        // (about 900 billion) are exact; smallmoney always is.
        assert_eq!(
            max[col("little_cash")],
            Value::Decimal("214748.3647".into())
        );
        assert_eq!(max[col("born")], Value::Text("2024-02-29".into()));
        assert_eq!(
            max[col("at_dt2")],
            Value::Text("9999-12-31 23:59:59.999999900".into())
        );
        assert_eq!(
            max[col("uid")],
            Value::Text("A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11".into())
        );
        assert_eq!(max[col("bytes")], Value::Blob(vec![0xde, 0xad, 0xbe, 0xef]));
        assert!(matches!(&max[col("doc")], Value::Text(xml) if xml.contains("🦀")));
        assert_eq!(
            zoo.columns[col("uid")].type_name.as_deref(),
            Some("uniqueidentifier")
        );
        assert!(zoo.rows[2].iter().skip(1).all(Value::is_null));

        let odd = rows(
            &conn,
            &format!(
                "select * from {}",
                conn.quote_ident("Order Items (archive)")
            ),
        );
        assert_eq!(odd.rows[0][4], Value::Text("🦀🐘🐬".into()));

        let result = conn
            .execute("select 1 as one; select 2 as two", 10)
            .unwrap();
        assert_eq!(result.statements, 2);

        let affected = conn
            .execute("update customers set city = city where id <= 3", 10)
            .unwrap();
        assert!(
            matches!(affected.outcome, QueryOutcome::Affected(3)),
            "{:?}",
            affected.outcome
        );

        let plan = rows(
            &conn,
            &conn.explain("select * from orders where customer_id = 5"),
        );
        assert!(!plan.rows.is_empty());

        let err = conn
            .execute("select 1;\nselec * from orders", 10)
            .unwrap_err();
        let err = err.downcast_ref::<QueryError>().expect("a query error");
        assert_eq!(err.position, Some(11), "{}", err.message);

        // Row limits: cut off, and the next query still works (on a new session).
        let limited = conn.execute("select * from events", 50).unwrap();
        assert!(
            matches!(&limited.outcome, QueryOutcome::Rows(r) if r.truncated && r.rows.len() == 50)
        );
        assert_eq!(
            rows(&conn, "select count(*) from orders").rows[0][0],
            Value::Integer(3000)
        );

        // Cancel ends a long query; the next one reconnects.
        let cancel = conn.cancel_handle();
        let timer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            cancel.cancel();
        });
        let started = Instant::now();
        let waited = conn.execute("waitfor delay '00:00:20'", 10);
        timer.join().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "cancel didn't stop the query"
        );
        assert!(waited.is_err());
        assert_eq!(
            rows(&conn, "select count(*) from orders").rows[0][0],
            Value::Integer(3000)
        );
    }
}
