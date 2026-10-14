//! Finding databases already on this Mac: Docker containers, servers listening
//! on the usual local ports, `~/.pgpass`, `~/.pg_service.conf`, and database
//! URLs in a project's `.env` files.

use std::collections::HashSet;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use xenon_driver::{ConnectOptions, SslMode};
use xenon_store::ServerEngine;

use crate::detect::{Detected, detect};
use crate::servers;

#[derive(Clone, Debug, PartialEq)]
// Only a handful exist at a time; boxing the options would buy nothing.
#[allow(clippy::large_enum_variant)]
pub enum FoundTarget {
    Server(ServerEngine, ConnectOptions),
    /// An engine Xenon can't open yet.
    NotYet(&'static str),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    /// Where it was found, e.g. "Docker · billing-db" or "~/dev/app/.env".
    pub source: String,
    pub name: String,
    pub target: FoundTarget,
}

impl Found {
    pub fn engine(&self) -> &'static str {
        match &self.target {
            FoundTarget::Server(engine, _) => engine.label(),
            FoundTarget::NotYet(engine) => engine,
        }
    }

    /// `user@host:port/db`, without the password.
    pub fn detail(&self) -> String {
        match &self.target {
            FoundTarget::Server(engine, o) => servers::describe_options(*engine, o),
            FoundTarget::NotYet(_) => self.name.clone(),
        }
    }

    /// True when it has everything needed to connect; otherwise the
    /// connection form opens pre-filled.
    /// Local servers are tried as they are (they often trust local users);
    /// remote ones need a password first.
    pub fn ready(&self) -> bool {
        matches!(&self.target, FoundTarget::Server(_, o) if !o.password.is_empty() || is_local(&o.host))
    }

    fn key(&self) -> String {
        format!("{}|{}", self.engine(), self.detail())
    }
}

/// Everything found on this machine (not `.env` files; see [`scan_env_files`]).
/// Runs commands and opens sockets, so call it off the main thread.
pub fn discover() -> Vec<Found> {
    let mut found = docker();
    found.extend(pgpass(&home(".pgpass")));
    found.extend(pg_services(&home(".pg_service.conf")));
    let taken: HashSet<u16> = found
        .iter()
        .filter_map(|f| match &f.target {
            FoundTarget::Server(_, o) if is_local(&o.host) => o.port,
            _ => None,
        })
        .collect();
    found.extend(listening_ports(&taken));
    dedupe(found)
}

fn home(file: &str) -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(file)
}

fn is_local(host: &str) -> bool {
    matches!(host, "" | "localhost" | "127.0.0.1" | "::1")
}

fn dedupe(found: Vec<Found>) -> Vec<Found> {
    let mut seen = HashSet::new();
    found.into_iter().filter(|f| seen.insert(f.key())).collect()
}

// ------------------------------------------------------------------ Docker

/// Run a command, giving up after `timeout` (Docker hangs when its daemon is down).
fn run(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => return None,
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => return None,
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    Some(out)
}

fn docker() -> Vec<Found> {
    let timeout = Duration::from_secs(2);
    let Some(listing) = run(
        "docker",
        &["ps", "--format", "{{.Names}}\t{{.Image}}\t{{.Ports}}"],
        timeout,
    ) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for line in listing.lines() {
        let mut parts = line.splitn(3, '\t');
        let (Some(name), Some(image), Some(ports)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let Some((engine, port)) = engine_for(image, ports) else {
            continue;
        };
        let source = format!("Docker · {name}");
        if let Some(server) = ServerEngine::ALL.into_iter().find(|e| e.label() == engine) {
            let env = run(
                "docker",
                &[
                    "inspect",
                    "--format",
                    "{{range .Config.Env}}{{println .}}{{end}}",
                    name,
                ],
                timeout,
            )
            .unwrap_or_default();
            found.push(Found {
                source,
                name: name.to_string(),
                target: FoundTarget::Server(server, options_from_env(server, &env, port)),
            });
        } else {
            found.push(Found {
                source,
                name: format!("{name} (localhost:{port})"),
                target: FoundTarget::NotYet(engine),
            });
        }
    }
    found
}

/// Which engine a container runs and the host port it's published on.
fn engine_for(image: &str, ports: &str) -> Option<(&'static str, u16)> {
    let image = image.to_ascii_lowercase();
    let candidates: [(&str, &[&str], u16); 3] = [
        (
            "Postgres",
            &[
                "postgres",
                "pgvector",
                "postgis",
                "timescale",
                "supabase/postgres",
            ],
            5432,
        ),
        ("MySQL", &["mysql", "mariadb", "percona"], 3306),
        (
            "SQL Server",
            &["mssql", "sqlserver", "azure-sql-edge"],
            1433,
        ),
    ];
    for (engine, images, container_port) in candidates {
        if images.iter().any(|i| image.contains(i)) {
            return published_port(ports, container_port).map(|port| (engine, port));
        }
    }
    // Unknown image, but a well-known port is published.
    candidates
        .iter()
        .find_map(|(engine, _, port)| published_port(ports, *port).map(|p| (*engine, p)))
}

/// "0.0.0.0:5433->5432/tcp, :::5433->5432/tcp" → Some(5433) for 5432.
fn published_port(ports: &str, container_port: u16) -> Option<u16> {
    ports.split(", ").find_map(|mapping| {
        let (host, container) = mapping.split_once("->")?;
        (container.split('/').next()? == container_port.to_string())
            .then(|| host.rsplit(':').next()?.parse().ok())?
    })
}

/// Credentials from the official images' environment variables.
fn options_from_env(engine: ServerEngine, env: &str, port: u16) -> ConnectOptions {
    let var = |key: &str| {
        env.lines()
            .find_map(|line| line.strip_prefix(&format!("{key}=")))
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let (user, password, database) = match engine {
        ServerEngine::Postgres => {
            let user = var("POSTGRES_USER").unwrap_or_else(|| "postgres".into());
            let database = var("POSTGRES_DB").unwrap_or_else(|| user.clone());
            (user, var("POSTGRES_PASSWORD"), database)
        }
        // MySQL and MariaDB images: an app user if one was made, else root.
        ServerEngine::Mysql => {
            let pick = |mysql: &str, mariadb: &str| var(mysql).or_else(|| var(mariadb));
            let database = pick("MYSQL_DATABASE", "MARIADB_DATABASE").unwrap_or_default();
            match pick("MYSQL_USER", "MARIADB_USER") {
                Some(user) => (user, pick("MYSQL_PASSWORD", "MARIADB_PASSWORD"), database),
                None => (
                    "root".into(),
                    pick("MYSQL_ROOT_PASSWORD", "MARIADB_ROOT_PASSWORD"),
                    database,
                ),
            }
        }
        // The official image only has the sa login.
        ServerEngine::SqlServer => (
            "sa".into(),
            var("MSSQL_SA_PASSWORD").or_else(|| var("SA_PASSWORD")),
            String::new(),
        ),
    };
    ConnectOptions {
        host: "localhost".into(),
        port: Some(port),
        database,
        password: password.unwrap_or_default(),
        user,
        ssl: SslMode::Prefer,
        read_only: false,
        ssh: None,
        address: None,
    }
}

// --------------------------------------------------------- local servers

const PORTS: [(u16, &str); 8] = [
    (5432, "Postgres"),
    (5433, "Postgres"),
    (5434, "Postgres"),
    (5435, "Postgres"),
    (3306, "MySQL"),
    (3307, "MySQL"),
    (1433, "SQL Server"),
    (54330, "Postgres"),
];

fn listening_ports(taken: &HashSet<u16>) -> Vec<Found> {
    PORTS
        .iter()
        .filter(|(port, _)| !taken.contains(port))
        .filter(|(port, _)| {
            let address = SocketAddr::from(([127, 0, 0, 1], *port));
            TcpStream::connect_timeout(&address, Duration::from_millis(150)).is_ok()
        })
        .map(|(port, engine)| Found {
            source: format!("Listening on localhost:{port}"),
            name: format!("localhost:{port}"),
            target: match ServerEngine::ALL.into_iter().find(|e| e.label() == *engine) {
                Some(server) => FoundTarget::Server(
                    server,
                    ConnectOptions {
                        host: "localhost".into(),
                        port: Some(*port),
                        ..Default::default()
                    },
                ),
                None => FoundTarget::NotYet(engine),
            },
        })
        .collect()
}

// ----------------------------------------------------- Postgres configs

fn pgpass(path: &Path) -> Vec<Found> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    parse_pgpass(&text, &crate::detect::tilde(&path.display().to_string()))
}

/// `host:port:database:user:password` lines; `*` hosts and comments skipped.
fn parse_pgpass(text: &str, source: &str) -> Vec<Found> {
    text.lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .filter_map(|line| {
            let fields: Vec<String> = split_pgpass(line);
            let [host, port, database, user, password] = fields.as_slice() else {
                return None;
            };
            if host == "*" {
                return None;
            }
            let database = if database == "*" {
                user.clone()
            } else {
                database.clone()
            };
            Some(Found {
                source: source.to_string(),
                name: format!("{database}@{host}"),
                target: FoundTarget::Server(
                    ServerEngine::Postgres,
                    ConnectOptions {
                        host: host.clone(),
                        port: port.parse().ok(),
                        database,
                        user: if user == "*" {
                            String::new()
                        } else {
                            user.clone()
                        },
                        password: password.clone(),
                        ssl: SslMode::Prefer,
                        read_only: false,
                        ssh: None,
                        address: None,
                    },
                ),
            })
        })
        .collect()
}

/// Split on `:`, honouring `\:` and `\\` escapes.
fn split_pgpass(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    fields.last_mut().unwrap().push(next);
                }
            }
            ':' => fields.push(String::new()),
            c => fields.last_mut().unwrap().push(c),
        }
    }
    fields
}

fn pg_services(path: &Path) -> Vec<Found> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    parse_pg_services(&text, &crate::detect::tilde(&path.display().to_string()))
}

/// INI-style `[service]` sections of `host=`, `port=`, `dbname=`, `user=`, `password=`.
fn parse_pg_services(text: &str, source: &str) -> Vec<Found> {
    let mut found = Vec::new();
    let mut current: Option<(String, ConnectOptions)> = None;
    let finish = |current: &mut Option<(String, ConnectOptions)>, found: &mut Vec<Found>| {
        if let Some((name, options)) = current.take() {
            found.push(Found {
                source: format!("{source} [{name}]"),
                name,
                target: FoundTarget::Server(ServerEngine::Postgres, options),
            });
        }
    };
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            finish(&mut current, &mut found);
            current = Some((name.to_string(), ConnectOptions::default()));
        } else if let (Some((_, options)), Some((key, value))) =
            (&mut current, line.split_once('='))
        {
            let value = value.trim().to_string();
            match key.trim() {
                "host" => options.host = value,
                "port" => options.port = value.parse().ok(),
                "dbname" => options.database = value,
                "user" => options.user = value,
                "password" => options.password = value,
                "sslmode" => options.ssl = SslMode::parse(&value).unwrap_or_default(),
                _ => {}
            }
        }
    }
    finish(&mut current, &mut found);
    found
}

// ----------------------------------------------------------- .env files

const SKIP_DIRS: [&str; 8] = [
    "node_modules",
    ".git",
    "target",
    "vendor",
    "dist",
    "build",
    ".venv",
    ".next",
];

/// Database URLs in `.env*` files under `root` (up to four levels deep).
pub fn scan_env_files(root: &Path) -> Vec<Found> {
    let mut found = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if depth < 4 && !SKIP_DIRS.contains(&name.as_str()) {
                    stack.push((path, depth + 1));
                }
            } else if (name == ".env" || name.starts_with(".env."))
                && !name.ends_with(".example")
                && let Ok(text) = std::fs::read_to_string(&path)
            {
                found.extend(parse_env_file(
                    &text,
                    &crate::detect::tilde(&path.display().to_string()),
                ));
            }
        }
    }
    dedupe(found)
}

fn parse_env_file(text: &str, source: &str) -> Vec<Found> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with('#') || !line.contains("://") {
                return None;
            }
            let key = line
                .trim_start_matches("export ")
                .split('=')
                .next()?
                .trim()
                .to_string();
            let target = match detect(line) {
                Detected::Server(engine, url) => {
                    FoundTarget::Server(engine, servers::parse_url(engine, &url).ok()?)
                }
                // Caches and document stores aren't what a SQL client is looking for.
                Detected::NotYet("Redis" | "MongoDB") => return None,
                Detected::NotYet(engine) => FoundTarget::NotYet(engine),
                _ => return None,
            };
            Some(Found {
                source: format!("{source} · {key}"),
                name: key,
                target,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_ports_and_images() {
        let ports = "0.0.0.0:5433->5432/tcp, :::5433->5432/tcp";
        assert_eq!(published_port(ports, 5432), Some(5433));
        assert_eq!(published_port(ports, 3306), None);
        assert_eq!(published_port("5432/tcp", 5432), None, "not published");
        assert_eq!(
            engine_for("pgvector/pgvector:pg17", ports),
            Some(("Postgres", 5433))
        );
        assert_eq!(
            engine_for("mariadb:11", "127.0.0.1:3306->3306/tcp"),
            Some(("MySQL", 3306))
        );
        assert_eq!(
            engine_for("company/custom-db", "0.0.0.0:6000->5432/tcp"),
            Some(("Postgres", 6000))
        );
        assert_eq!(engine_for("redis:7", "0.0.0.0:6379->6379/tcp"), None);
    }

    #[test]
    fn postgres_container_env() {
        let env =
            "PATH=/usr/bin\nPOSTGRES_USER=app\nPOSTGRES_PASSWORD=secret\nPOSTGRES_DB=billing\n";
        let options = options_from_env(ServerEngine::Postgres, env, 5433);
        assert_eq!(
            (
                options.user.as_str(),
                options.password.as_str(),
                options.database.as_str(),
                options.port
            ),
            ("app", "secret", "billing", Some(5433))
        );
        let defaults = options_from_env(ServerEngine::Postgres, "POSTGRES_PASSWORD=pw", 5432);
        assert_eq!(
            (defaults.user.as_str(), defaults.database.as_str()),
            ("postgres", "postgres")
        );
    }

    #[test]
    fn mysql_container_env() {
        let app = options_from_env(
            ServerEngine::Mysql,
            "MYSQL_ROOT_PASSWORD=root\nMYSQL_USER=app\nMYSQL_PASSWORD=pw\nMYSQL_DATABASE=shop\n",
            53306,
        );
        assert_eq!(
            (
                app.user.as_str(),
                app.password.as_str(),
                app.database.as_str()
            ),
            ("app", "pw", "shop")
        );
        let root = options_from_env(ServerEngine::Mysql, "MARIADB_ROOT_PASSWORD=r00t\n", 3306);
        assert_eq!(
            (
                root.user.as_str(),
                root.password.as_str(),
                root.database.as_str()
            ),
            ("root", "r00t", "")
        );
    }

    #[test]
    fn sql_server_container_env() {
        let options = options_from_env(
            ServerEngine::SqlServer,
            "ACCEPT_EULA=Y\nMSSQL_SA_PASSWORD=Str0ng!\n",
            51433,
        );
        assert_eq!(
            (
                options.user.as_str(),
                options.password.as_str(),
                options.port
            ),
            ("sa", "Str0ng!", Some(51433))
        );
    }

    #[test]
    fn pgpass_lines() {
        let found = parse_pgpass(
            "# comment\nlocalhost:5432:app:me:pa\\:ss\n*:*:*:me:x\ndb.example.com:6543:*:svc:pw\n",
            "~/.pgpass",
        );
        assert_eq!(found.len(), 2);
        let FoundTarget::Server(_, first) = &found[0].target else {
            panic!()
        };
        assert_eq!(
            (first.database.as_str(), first.password.as_str()),
            ("app", "pa:ss")
        );
        assert_eq!(found[1].detail(), "svc@db.example.com:6543/svc");
    }

    #[test]
    fn pg_service_sections() {
        let found = parse_pg_services(
            "[reporting]\nhost=replica.internal\nport=5433\ndbname=reports\nuser=analyst\n\n[local]\ndbname=dev\n",
            "~/.pg_service.conf",
        );
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].name, "reporting");
        assert_eq!(found[0].detail(), "analyst@replica.internal:5433/reports");
        assert_eq!(
            found[1].detail(),
            format!("{}@localhost:5432/dev", xenon_postgres::default_user())
        );
    }

    #[test]
    fn env_file_urls() {
        let found = parse_env_file(
            "# db\nDATABASE_URL=postgres://u:p@localhost:5432/app\nexport CACHE_URL=redis://x\nANALYTICS_DB=\"mysql://root@db/stats\"\nNOT_A_URL=hello\n",
            "~/dev/app/.env",
        );
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].name, "DATABASE_URL");
        assert!(found[0].ready());
        assert!(matches!(
            &found[1].target,
            FoundTarget::Server(ServerEngine::Mysql, _)
        ));
        assert_eq!(found[1].detail(), "root@db:3306/stats");
    }

    #[test]
    fn scans_env_files_skipping_dependencies() {
        let root = std::env::temp_dir().join(format!("xenon-env-scan-{}", std::process::id()));
        std::fs::create_dir_all(root.join("api")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::write(
            root.join("api/.env.local"),
            "DATABASE_URL=postgres://a@localhost/api\n",
        )
        .unwrap();
        std::fs::write(
            root.join("node_modules/pkg/.env"),
            "DATABASE_URL=postgres://b@localhost/no\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".env.example"),
            "DATABASE_URL=postgres://c@localhost/example\n",
        )
        .unwrap();
        let found = scan_env_files(&root);
        assert_eq!(found.len(), 1);
        assert!(
            found[0].source.ends_with("api/.env.local · DATABASE_URL"),
            "{}",
            found[0].source
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}

#[cfg(test)]
mod live {
    /// Prints what this machine has: `cargo test -p xenon_ui discover_here -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn discover_here() {
        let started = std::time::Instant::now();
        for found in super::discover() {
            println!(
                "{:<40} {:<10} {} ready={}",
                found.source,
                found.engine(),
                found.detail(),
                found.ready()
            );
        }
        println!("took {:?}", started.elapsed());
    }
}
