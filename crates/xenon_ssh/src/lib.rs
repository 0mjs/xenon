//! SSH tunnels, made by running the system's `ssh` client.
//!
//! Using OpenSSH itself means everything people already rely on just works:
//! `~/.ssh/config` aliases and `ProxyJump`, the agent (1Password, Secretive,
//! hardware keys) and `known_hosts`. Xenon records host keys it hasn't seen in
//! its own `known_hosts` (so yours is never written to) while still trusting
//! every host in `~/.ssh/known_hosts`.

use std::io::Read as _;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
pub use xenon_driver::SshOptions;

/// How long to wait for the SSH login and the forwarded port.
const READY_TIMEOUT: Duration = Duration::from_secs(20);

/// A local port forwarded through an SSH server to a database. The `ssh`
/// process lives as long as this value.
pub struct Tunnel {
    child: Child,
    local_port: u16,
    log: Arc<Mutex<String>>,
    destination: String,
}

impl Tunnel {
    /// Log in to `ssh` and forward a free local port to `target_host:target_port`
    /// (as seen from the SSH server). Blocks until the tunnel is ready.
    /// New host keys are recorded in `known_hosts`.
    pub fn open(
        ssh: &SshOptions,
        target_host: &str,
        target_port: u16,
        known_hosts: &Path,
    ) -> Result<Self> {
        if ssh.host.trim().is_empty() {
            return Err(anyhow!("The SSH tunnel needs a host."));
        }
        let local_port = free_port()?;
        let target_host = if target_host.contains(':') {
            format!("[{target_host}]")
        } else {
            target_host.to_string()
        };
        if let Some(dir) = known_hosts.parent() {
            std::fs::create_dir_all(dir)?;
        }

        let mut command = Command::new(ssh_program());
        command
            .args(["-N", "-T"])
            .args(["-o", "ExitOnForwardFailure=yes"])
            .args(["-o", "ConnectTimeout=10"])
            .args([
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
            ])
            .args(["-o", "StrictHostKeyChecking=accept-new"])
            .arg("-o")
            .arg(format!(
                "UserKnownHostsFile=\"{}\" ~/.ssh/known_hosts",
                known_hosts.display()
            ))
            // INFO, not ERROR: "channel 2: open failed: …" (the database host
            // can't be reached from the SSH server) is logged at INFO.
            .args(["-o", "LogLevel=INFO"])
            .arg("-L")
            .arg(format!(
                "127.0.0.1:{local_port}:{target_host}:{target_port}"
            ));
        if let Some(port) = ssh.port {
            command.arg("-p").arg(port.to_string());
        }
        if !ssh.user.trim().is_empty() {
            command.arg("-l").arg(ssh.user.trim());
        }
        if !ssh.key_file.trim().is_empty() {
            // Only this key: offering every agent key first can hit the
            // server's limit on attempts before this one is tried.
            command
                .arg("-i")
                .arg(expand_home(ssh.key_file.trim()))
                .args(["-o", "IdentitiesOnly=yes"]);
        }

        // A password (or key passphrase) is handed over by a one-line askpass
        // script reading it from the environment; with none, ssh must never prompt.
        let askpass = if ssh.password.is_empty() {
            command.args(["-o", "BatchMode=yes"]);
            None
        } else {
            let script = Askpass::create()?;
            command
                .env("SSH_ASKPASS", &script.0)
                .env("SSH_ASKPASS_REQUIRE", "force")
                .env("XENON_SSH_SECRET", &ssh.password)
                .args(["-o", "NumberOfPasswordPrompts=1"]);
            Some(script)
        };

        command
            .arg(ssh.host.trim())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|err| anyhow!("Couldn't run ssh: {err}"))?;

        let log = Arc::new(Mutex::new(String::new()));
        if let Some(mut stderr) = child.stderr.take() {
            let log = log.clone();
            std::thread::spawn(move || {
                let mut buffer = [0u8; 1024];
                while let Ok(n) = stderr.read(&mut buffer) {
                    if n == 0 {
                        break;
                    }
                    let mut log = log.lock().unwrap_or_else(|e| e.into_inner());
                    log.push_str(&String::from_utf8_lossy(&buffer[..n]));
                }
            });
        }

        let destination = match ssh.port {
            Some(port) => format!("{}:{port}", ssh.host.trim()),
            None => ssh.host.trim().to_string(),
        };
        let mut tunnel = Self {
            child,
            local_port,
            log,
            destination,
        };
        let ready = tunnel.wait_until_ready();
        drop(askpass);
        ready.map(|()| tunnel)
    }

    /// The port on 127.0.0.1 to connect the database driver to.
    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    /// What ssh has reported so far, for error messages.
    pub fn log(&self) -> String {
        self.log
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .trim()
            .to_string()
    }

    /// ssh's explanation, once it has one, of why a forwarded connection
    /// failed: e.g. "connect failed: Name does not resolve". Waits briefly,
    /// since ssh logs it just after the database client sees the reset.
    pub fn forward_failure(&self) -> Option<String> {
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            let log = self.log();
            if let Some(line) = log.lines().find(|line| line.contains("open failed")) {
                let reason = line.split("open failed: ").nth(1).unwrap_or(line);
                return Some(reason.trim().to_string());
            }
            if Instant::now() > deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Where it tunnels through, e.g. `bastion.example.com` or `jump:2222`.
    pub fn destination(&self) -> &str {
        &self.destination
    }

    fn wait_until_ready(&mut self) -> Result<()> {
        let started = Instant::now();
        let address = SocketAddr::from(([127, 0, 0, 1], self.local_port));
        loop {
            if let Some(status) = self.child.try_wait()? {
                // Give the stderr reader a moment to catch the last words.
                std::thread::sleep(Duration::from_millis(50));
                return Err(explain(&self.log(), &self.destination, status.code()));
            }
            if TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok() {
                return Ok(());
            }
            if started.elapsed() > READY_TIMEOUT {
                return Err(anyhow!(
                    "Timed out setting up the SSH tunnel through {}.{}",
                    self.destination,
                    suffix(&self.log())
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        ports_in_use()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.local_port);
    }
}

/// Turn ssh's last words into something a person can act on.
fn explain(log: &str, destination: &str, code: Option<i32>) -> anyhow::Error {
    let lower = log.to_ascii_lowercase();
    let message = if lower.contains("permission denied") {
        format!(
            "The SSH server {destination} didn't accept the login. Check the user, key or password."
        )
    } else if lower.contains("host key verification failed")
        || lower.contains("remote host identification has changed")
    {
        format!(
            "The SSH host key for {destination} has changed since it was last seen. If that's expected, \
             remove its line from known_hosts and connect again."
        )
    } else if lower.contains("could not resolve hostname") {
        format!("Couldn't find the SSH server {destination}.")
    } else if lower.contains("connection refused") {
        format!(
            "The SSH server {destination} refused the connection. Is sshd running on that port?"
        )
    } else if lower.contains("timed out") {
        format!("Timed out reaching the SSH server {destination}.")
    } else if lower.contains("address already in use") || lower.contains("cannot listen") {
        "Couldn't open a local port for the SSH tunnel; try again.".to_string()
    } else {
        format!(
            "The SSH tunnel through {destination} closed (ssh exited with {}).",
            code.map_or("a signal".to_string(), |c| format!("code {c}"))
        )
    };
    anyhow!("{message}{}", suffix(log))
}

fn suffix(log: &str) -> String {
    if log.is_empty() {
        String::new()
    } else {
        format!("\n\nssh: {log}")
    }
}

/// A port nobody is listening on right now.
/// Local ports handed to open tunnels: two tunnels opened at once must never
/// share one, or one would take the other's listener for its own.
fn ports_in_use() -> &'static Mutex<std::collections::HashSet<u16>> {
    static PORTS: std::sync::OnceLock<Mutex<std::collections::HashSet<u16>>> =
        std::sync::OnceLock::new();
    PORTS.get_or_init(Default::default)
}

fn free_port() -> Result<u16> {
    let mut taken = ports_in_use().lock().unwrap_or_else(|e| e.into_inner());
    loop {
        let port = TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port();
        if taken.insert(port) {
            return Ok(port);
        }
    }
}

/// Apps opened from the Finder get a minimal PATH; ssh is always at /usr/bin/ssh on macOS.
fn ssh_program() -> &'static str {
    if Path::new("/usr/bin/ssh").exists() {
        "/usr/bin/ssh"
    } else {
        "ssh"
    }
}

fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

/// A private, temporary askpass script; removed when dropped.
struct Askpass(PathBuf);

impl Askpass {
    fn create() -> Result<Self> {
        use std::os::unix::fs::OpenOptionsExt as _;
        static COUNT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "xenon-askpass-{}-{}.sh",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&path)?;
        std::io::Write::write_all(
            &mut file,
            b"#!/bin/sh\nprintf '%s\\n' \"$XENON_SSH_SECRET\"\n",
        )?;
        Ok(Self(path))
    }
}

impl Drop for Askpass {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_say_what_to_do() {
        let denied = explain(
            "xenon@host: Permission denied (publickey).",
            "bastion",
            Some(255),
        );
        assert!(
            denied
                .to_string()
                .starts_with("The SSH server bastion didn't accept the login")
        );
        let unknown = explain(
            "ssh: Could not resolve hostname nope: nodename nor servname provided",
            "nope",
            Some(255),
        );
        assert!(
            unknown
                .to_string()
                .starts_with("Couldn't find the SSH server nope.")
        );
        assert!(explain("", "b", None).to_string().contains("closed"));
    }

    #[test]
    fn a_missing_host_is_an_error() {
        let err = Tunnel::open(&SshOptions::default(), "db", 5432, Path::new("/tmp/kh"))
            .err()
            .unwrap();
        assert_eq!(err.to_string(), "The SSH tunnel needs a host.");
    }
}

/// Against the test fleet's bastion: `dev/fleet/up.sh bastion postgres`, then
/// `cargo test -p xenon_ssh -- --ignored`.
#[cfg(test)]
mod live {
    use super::*;

    /// Tunnels to one bastion: one test at a time.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn known_hosts() -> PathBuf {
        std::env::temp_dir().join(format!("xenon-test-known-hosts-{}", std::process::id()))
    }

    fn bastion() -> SshOptions {
        SshOptions {
            host: "127.0.0.1".into(),
            port: Some(52222),
            user: "xenon".into(),
            ..Default::default()
        }
    }

    fn key() -> String {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../dev/fleet/ssh/id_ed25519"
        )
        .to_string()
    }

    /// Postgres answers an SSLRequest with one byte, `S` or `N`.
    fn postgres_answers(port: u16) -> bool {
        use std::io::Write as _;
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(&[0, 0, 0, 8, 4, 210, 22, 47]).unwrap();
        let mut reply = [0u8; 1];
        stream.read_exact(&mut reply).is_ok() && matches!(&reply, b"S" | b"N")
    }

    #[test]
    #[ignore]
    fn key_login() {
        let _serial = serial();
        let tunnel = Tunnel::open(
            &SshOptions {
                key_file: key(),
                ..bastion()
            },
            "postgres",
            5432,
            &known_hosts(),
        )
        .unwrap();
        assert!(postgres_answers(tunnel.local_port()));
    }

    #[test]
    #[ignore]
    fn password_login() {
        let _serial = serial();
        let tunnel = Tunnel::open(
            &SshOptions {
                password: "xenon".into(),
                ..bastion()
            },
            "postgres",
            5432,
            &known_hosts(),
        )
        .unwrap();
        assert!(postgres_answers(tunnel.local_port()));
    }

    #[test]
    #[ignore]
    fn wrong_password_is_explained() {
        let _serial = serial();
        let err = Tunnel::open(
            &SshOptions {
                password: "nope".into(),
                ..bastion()
            },
            "postgres",
            5432,
            &known_hosts(),
        )
        .err()
        .unwrap();
        assert!(
            err.to_string().contains("didn't accept the login"),
            "{err:#}"
        );
    }

    #[test]
    #[ignore]
    fn a_new_host_key_is_recorded_and_a_changed_one_refused() {
        let _serial = serial();
        let known =
            std::env::temp_dir().join(format!("xenon-test-host-keys-{}", std::process::id()));
        let _ = std::fs::remove_file(&known);
        let options = SshOptions {
            key_file: key(),
            ..bastion()
        };
        // First visit: accepted, and the key is written down.
        drop(Tunnel::open(&options, "postgres", 5432, &known).unwrap());
        // (Found with ssh-keygen: ssh may store host names hashed.)
        let found = std::process::Command::new("ssh-keygen")
            .args(["-F", "[127.0.0.1]:52222", "-f"])
            .arg(&known)
            .output()
            .unwrap();
        assert!(
            found.status.success() && !found.stdout.is_empty(),
            "the key wasn't recorded"
        );
        // Someone else answers as the bastion: a different key is refused.
        let impostor =
            std::env::temp_dir().join(format!("xenon-test-impostor-{}", std::process::id()));
        let _ = std::fs::remove_file(&impostor);
        let made = std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&impostor)
            .status()
            .unwrap();
        assert!(made.success());
        let public = std::fs::read_to_string(impostor.with_extension("pub")).unwrap();
        let key_part: Vec<&str> = public.split_whitespace().take(2).collect();
        std::fs::write(
            &known,
            format!("[127.0.0.1]:52222 {}\n", key_part.join(" ")),
        )
        .unwrap();
        let err = Tunnel::open(&options, "postgres", 5432, &known)
            .err()
            .unwrap();
        assert!(err.to_string().contains("host key"), "{err:#}");
        for path in [&known, &impostor, &impostor.with_extension("pub")] {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    #[ignore]
    fn closing_the_tunnel_stops_ssh() {
        let _serial = serial();
        let tunnel = Tunnel::open(
            &SshOptions {
                key_file: key(),
                ..bastion()
            },
            "postgres",
            5432,
            &known_hosts(),
        )
        .unwrap();
        let port = tunnel.local_port();
        drop(tunnel);
        std::thread::sleep(Duration::from_millis(100));
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
    }
}
