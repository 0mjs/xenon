//! The connection editor: connect to Postgres (paste a URL or fill in the
//! fields), or edit a saved connection's name, environment and read-only flag.

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, FontWeight, IntoElement, Render, SharedString,
    Subscription, Window, div, prelude::*, px,
};
use gpui_component::input::{Escape, InputEvent, InputState};
use xenon_driver::{ConnectOptions, SslMode};
use xenon_store::{Environment, SavedConnection, ServerEngine, Target};

use crate::servers;
use crate::theme::{ActiveTheme, Theme};
use crate::widgets::{self, ButtonKind, Engine, RADIUS_CONTROL};

/// What to show in an empty Database field.
fn database_hint(engine: ServerEngine) -> &'static str {
    match engine {
        ServerEngine::Postgres => "postgres",
        ServerEngine::Mysql | ServerEngine::SqlServer => "optional",
    }
}

/// What the dialog is for.
#[derive(Clone)]
pub enum Subject {
    /// A new server connection, starting on this engine.
    New(ServerEngine),
    /// A saved connection, with its database and SSH passwords from the Keychain.
    Saved(SavedConnection, String, String),
    /// A new connection with fields filled in (from discovery or a paste).
    Prefill {
        engine: ServerEngine,
        name: String,
        options: ConnectOptions,
    },
}

/// A filled-in dialog: where to connect and how to remember it.
#[derive(Clone)]
pub struct Request {
    /// Id of the saved connection being edited, or 0 for a new one.
    pub id: i64,
    pub name: String,
    pub environment: Environment,
    pub read_only: bool,
    /// See [`widgets::CONNECTION_COLORS`]; empty for none.
    pub color: String,
    pub save: bool,
    pub target: RequestTarget,
}

#[derive(Clone)]
// Only a handful exist at a time; boxing the options would buy nothing.
#[allow(clippy::large_enum_variant)]
pub enum RequestTarget {
    Server(ServerEngine, ConnectOptions),
    Sqlite(String),
}

pub enum ConnectEvent {
    Submit(Request),
    Test(Request),
    Dismiss,
}

enum Status {
    Idle,
    Busy(&'static str),
    Failed(SharedString),
    Passed(SharedString),
}

pub struct ConnectDialog {
    focus_handle: FocusHandle,
    id: i64,
    /// Set when editing a saved SQLite file: no server fields.
    sqlite_path: Option<String>,
    engine: ServerEngine,
    name: Entity<InputState>,
    url: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    database: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    ssl: SslMode,
    /// The user picked the SSL mode, so choosing Production leaves it alone.
    ssl_chosen: bool,
    /// Tunnel through SSH, with these settings.
    ssh: bool,
    ssh_host: Entity<InputState>,
    ssh_port: Entity<InputState>,
    ssh_user: Entity<InputState>,
    ssh_key: Entity<InputState>,
    /// PEM files: a private CA to trust, a client certificate and its key.
    tls_ca: Entity<InputState>,
    tls_cert: Entity<InputState>,
    tls_key: Entity<InputState>,
    ssh_password: Entity<InputState>,
    environment: Environment,
    read_only: bool,
    color: String,
    save: bool,
    status: Status,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ConnectEvent> for ConnectDialog {}

impl ConnectDialog {
    pub fn new(subject: Subject, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let field = |placeholder: &'static str,
                     value: String,
                     window: &mut Window,
                     cx: &mut Context<Self>| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };

        let (id, saved_name, sqlite_path, engine, options, environment, read_only) = match &subject
        {
            Subject::New(engine) => (
                0,
                String::new(),
                None,
                *engine,
                ConnectOptions {
                    // As SQL Server's own tools: encrypt and check the certificate.
                    ssl: if *engine == ServerEngine::SqlServer {
                        SslMode::VerifyFull
                    } else {
                        SslMode::Prefer
                    },
                    ..ConnectOptions::default()
                },
                Environment::None,
                false,
            ),
            Subject::Prefill {
                engine,
                name,
                options,
            } => (
                0,
                name.clone(),
                None,
                *engine,
                options.clone(),
                Environment::None,
                false,
            ),
            Subject::Saved(saved, password, ssh_password) => {
                let (path, engine, options) = match &saved.target {
                    Target::Sqlite { path } => (
                        Some(path.clone()),
                        ServerEngine::Postgres,
                        ConnectOptions::default(),
                    ),
                    Target::Server {
                        engine,
                        host,
                        port,
                        database,
                        user,
                        ssl,
                        ssh,
                        tls,
                    } => (
                        None,
                        *engine,
                        ConnectOptions {
                            host: host.clone(),
                            port: *port,
                            database: database.clone(),
                            user: user.clone(),
                            password: password.clone(),
                            ssl: SslMode::parse(ssl).unwrap_or_default(),
                            read_only: saved.read_only,
                            address: None,
                            ssh: ssh.as_ref().map(|tunnel| xenon_driver::SshOptions {
                                password: ssh_password.clone(),
                                ..servers::ssh_options(tunnel)
                            }),
                            lock_timeout: None,
                            tls: servers::tls_files(tls),
                        },
                    ),
                };
                (
                    saved.id,
                    saved.name.clone(),
                    path,
                    engine,
                    options,
                    saved.environment,
                    saved.read_only,
                )
            }
        };

        let color = match &subject {
            Subject::Saved(saved, _, _) => saved.color.clone(),
            _ => String::new(),
        };
        let name = field(
            "e.g. Clients/Acme production (a slash files it in a folder)",
            saved_name,
            window,
            cx,
        );
        let url = field(servers::url_example(engine), String::new(), window, cx);
        let host = field("localhost", options.host.clone(), window, cx);
        let port = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(engine.default_port().to_string())
                .default_value(options.port.map(|p| p.to_string()).unwrap_or_default())
        });
        let database = field(database_hint(engine), options.database.clone(), window, cx);
        let user = field(engine.default_user(), options.user.clone(), window, cx);
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .default_value(options.password.clone())
        });

        let tunnel = options.ssh.clone().unwrap_or_default();
        let ssh_host = field(
            "bastion.example.com, or a Host from ~/.ssh/config",
            tunnel.host,
            window,
            cx,
        );
        let ssh_port = field(
            "22",
            tunnel.port.map(|p| p.to_string()).unwrap_or_default(),
            window,
            cx,
        );
        let ssh_user = field("from ~/.ssh/config", tunnel.user, window, cx);
        let ssh_key = field("agent or ~/.ssh/config", tunnel.key_file, window, cx);
        let tls_ca = field("~/certs/ca.pem", options.tls.ca.clone(), window, cx);
        let tls_cert = field("~/certs/client.pem", options.tls.cert.clone(), window, cx);
        let tls_key = field("~/certs/client.key", options.tls.key.clone(), window, cx);
        let ssh_password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("only if needed")
                .default_value(tunnel.password)
        });

        let subscriptions = [
            &name,
            &url,
            &host,
            &port,
            &database,
            &user,
            &password,
            &ssh_host,
            &ssh_port,
            &ssh_user,
            &ssh_key,
            &ssh_password,
        ]
        .into_iter()
        .map(|input| {
            cx.subscribe_in(input, window, |this, _, event: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.submit(cx);
                }
            })
        })
        .collect();

        // Start where typing is needed: the password when a pre-filled form
        // lacks one, the name when editing, otherwise the URL.
        let first = match &subject {
            Subject::Prefill { options, .. } if options.password.is_empty() => &password,
            Subject::Prefill { .. } => &name,
            _ if sqlite_path.is_some() || id != 0 => &name,
            _ => &url,
        };
        first.update(cx, |input, cx| input.focus(window, cx));

        Self {
            focus_handle: cx.focus_handle(),
            id,
            sqlite_path,
            engine,
            name,
            url,
            host,
            port,
            database,
            user,
            password,
            ssl: options.ssl,
            // An existing connection's mode was chosen; a new one's is the default.
            ssl_chosen: id != 0,
            environment,
            read_only,
            color,
            save: true,
            status: Status::Idle,
            ssh: options.ssh.is_some(),
            ssh_host,
            ssh_port,
            ssh_user,
            ssh_key,
            ssh_password,
            tls_ca,
            tls_cert,
            tls_key,
            _subscriptions: subscriptions,
        }
    }

    #[cfg(test)]
    pub(crate) fn show_ssh(&mut self, cx: &mut Context<Self>) {
        self.ssh = true;
        cx.notify();
    }

    /// Switch the form to another engine: its URL format and defaults.
    fn set_engine(&mut self, engine: ServerEngine, window: &mut Window, cx: &mut Context<Self>) {
        // SQL Server's own tools encrypt and check the certificate by default;
        // so does a new SQL Server connection here (Require to trust any).
        match (engine, self.ssl) {
            (ServerEngine::SqlServer, SslMode::Prefer) => self.ssl = SslMode::VerifyFull,
            // Only SQL Server's default goes back to the others' default: not
            // one chosen, nor Production's.
            (other, SslMode::VerifyFull)
                if other != ServerEngine::SqlServer
                    && self.engine == ServerEngine::SqlServer
                    && !self.ssl_chosen
                    && self.environment != Environment::Production =>
            {
                self.ssl = SslMode::Prefer
            }
            _ => {}
        }
        self.engine = engine;
        self.url.update(cx, |input, cx| {
            input.set_placeholder(servers::url_example(engine), window, cx)
        });
        self.port.update(cx, |input, cx| {
            input.set_placeholder(engine.default_port().to_string(), window, cx)
        });
        self.user.update(cx, |input, cx| {
            input.set_placeholder(engine.default_user(), window, cx)
        });
        self.database.update(cx, |input, cx| {
            input.set_placeholder(database_hint(engine), window, cx)
        });
        self.status = Status::Idle;
        cx.notify();
    }

    pub fn set_error(&mut self, error: SharedString, cx: &mut Context<Self>) {
        self.status = Status::Failed(error);
        cx.notify();
    }

    pub fn set_test_passed(&mut self, message: SharedString, cx: &mut Context<Self>) {
        self.status = Status::Passed(message);
        cx.notify();
    }

    /// Paste `url` and pick `ssl`, as a user would.
    #[cfg(test)]
    pub(crate) fn fill_for_test(
        &mut self,
        url: &str,
        ssl: SslMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.url
            .update(cx, |input, cx| input.set_value(url.to_string(), window, cx));
        self.ssl = ssl;
    }

    /// The server is on another machine (by the URL or the host field), and
    /// not reached through an SSH tunnel.
    pub(crate) fn remote(&self, cx: &App) -> bool {
        if self.ssh {
            return false;
        }
        let url = self.url.read(cx).value().trim().to_string();
        let host = if url.is_empty() {
            self.host.read(cx).value().trim().to_string()
        } else {
            servers::parse_url(self.engine, &url)
                .map(|options| options.host)
                .unwrap_or_default()
        };
        !(host.is_empty()
            || host.starts_with('/')
            || host.eq_ignore_ascii_case("localhost")
            || host.starts_with("127.")
            || host == "::1")
    }

    pub(crate) fn request(&mut self, cx: &mut Context<Self>) -> Option<Request> {
        let value =
            |input: &Entity<InputState>, cx: &App| input.read(cx).value().trim().to_string();
        let mut name = value(&self.name, cx);

        let target = if let Some(path) = &self.sqlite_path {
            RequestTarget::Sqlite(path.clone())
        } else {
            let url = value(&self.url, cx);
            // A pasted URL says which engine it's for.
            if let crate::detect::Detected::Server(engine, _) = crate::detect::detect(&url) {
                self.engine = engine;
            }
            let mut options = if !url.is_empty() {
                match servers::parse_url(self.engine, &url) {
                    // The form's TLS still counts: the stricter mode wins (a new
                    // production connection's verify-full isn't undone by a URL's
                    // `prefer`), and certificates the URL doesn't name come from
                    // the form.
                    Ok(mut options) => {
                        options.ssl = options.ssl.max(self.ssl);
                        for (field, input) in [
                            (&mut options.tls.ca, &self.tls_ca),
                            (&mut options.tls.cert, &self.tls_cert),
                            (&mut options.tls.key, &self.tls_key),
                        ] {
                            if field.is_empty() {
                                *field = value(input, cx);
                            }
                        }
                        options
                    }
                    Err(err) => {
                        self.set_error(format!("{err:#}").into(), cx);
                        return None;
                    }
                }
            } else {
                let port = value(&self.port, cx);
                let port = if port.is_empty() {
                    None
                } else if let Ok(port) = port.parse() {
                    Some(port)
                } else {
                    self.set_error(format!("\"{port}\" is not a valid port.").into(), cx);
                    return None;
                };
                ConnectOptions {
                    host: value(&self.host, cx),
                    port,
                    database: value(&self.database, cx),
                    user: value(&self.user, cx),
                    password: self.password.read(cx).value().to_string(),
                    ssl: self.ssl,
                    read_only: false,
                    ssh: None,
                    address: None,
                    lock_timeout: None,
                    tls: xenon_driver::TlsFiles {
                        ca: value(&self.tls_ca, cx),
                        cert: value(&self.tls_cert, cx),
                        key: value(&self.tls_key, cx),
                    },
                }
            };
            options.read_only = self.read_only;
            options.ssh = if self.ssh {
                let host = value(&self.ssh_host, cx);
                if host.is_empty() {
                    self.set_error("Enter the SSH server to tunnel through.".into(), cx);
                    return None;
                }
                let port = value(&self.ssh_port, cx);
                let port = if port.is_empty() {
                    None
                } else if let Ok(port) = port.parse() {
                    Some(port)
                } else {
                    self.set_error(format!("\"{port}\" is not a valid SSH port.").into(), cx);
                    return None;
                };
                Some(xenon_driver::SshOptions {
                    host,
                    port,
                    user: value(&self.ssh_user, cx),
                    key_file: value(&self.ssh_key, cx),
                    password: self.ssh_password.read(cx).value().to_string(),
                })
            } else {
                None
            };
            if name.is_empty() {
                name = servers::default_name(self.engine, &options);
            }
            RequestTarget::Server(self.engine, options)
        };

        Some(Request {
            id: self.id,
            name,
            environment: self.environment,
            color: self.color.clone(),
            read_only: self.read_only,
            save: self.save || self.id != 0,
            target,
        })
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if matches!(self.status, Status::Busy(_)) {
            return;
        }
        if let Some(request) = self.request(cx) {
            self.status = Status::Busy("Connecting…");
            cx.emit(ConnectEvent::Submit(request));
            cx.notify();
        }
    }

    fn test(&mut self, cx: &mut Context<Self>) {
        if matches!(self.status, Status::Busy(_)) {
            return;
        }
        if let Some(request) = self.request(cx) {
            self.status = Status::Busy("Testing…");
            cx.emit(ConnectEvent::Test(request));
            cx.notify();
        }
    }
}

impl Render for ConnectDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let editing = self.id != 0;
        let is_sqlite = self.sqlite_path.is_some();

        let labelled = |label: &'static str, input: &Entity<InputState>, focused: bool| {
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .flex_1()
                .min_w(px(0.))
                .child(widgets::field_label(&theme, label))
                .child(widgets::text_field(&theme, input, focused))
        };
        let focused = |input: &Entity<InputState>| widgets::is_focused(input, window, cx);
        let name_focused = focused(&self.name);
        let url_focused = focused(&self.url);
        let host_focused = focused(&self.host);
        let port_focused = focused(&self.port);
        let database_focused = focused(&self.database);
        let user_focused = focused(&self.user);
        let password_focused = focused(&self.password);
        let tls_focus = [
            focused(&self.tls_ca),
            focused(&self.tls_cert),
            focused(&self.tls_key),
        ];
        let ssh_focus = [
            focused(&self.ssh_host),
            focused(&self.ssh_port),
            focused(&self.ssh_user),
            focused(&self.ssh_key),
            focused(&self.ssh_password),
        ];

        let ssl_picker = widgets::segmented(
            &theme,
            "ssl",
            SslMode::ALL
                .into_iter()
                .map(|m| (m.as_str(), m == self.ssl))
                .collect(),
            cx.listener(|this, ix: &usize, _, cx| {
                this.ssl = SslMode::ALL[*ix];
                this.ssl_chosen = true;
                cx.notify();
            }),
        );
        let environment_picker = widgets::segmented(
            &theme,
            "environment",
            Environment::ALL
                .into_iter()
                .map(|e| {
                    let label = match e {
                        Environment::None => "None",
                        Environment::Development => "Development",
                        Environment::Production => "Production",
                    };
                    (label, e == self.environment)
                })
                .collect(),
            cx.listener(|this, ix: &usize, _, cx| {
                this.environment = Environment::ALL[*ix];
                // Production should check who it's talking to.
                if this.environment == Environment::Production && !this.ssl_chosen {
                    this.ssl = SslMode::VerifyFull;
                }
                cx.notify();
            }),
        );
        let access_picker = widgets::segmented(
            &theme,
            "access",
            vec![
                ("Read & write", !self.read_only),
                ("Read-only", self.read_only),
            ],
            cx.listener(|this, ix: &usize, _, cx| {
                this.read_only = *ix == 1;
                cx.notify();
            }),
        );

        let title: SharedString = match (editing, is_sqlite) {
            (true, _) | (_, true) => "Edit connection".into(),
            (false, false) => format!("Connect to {}", self.engine.label()).into(),
        };
        let subtitle = match (editing, is_sqlite) {
            (_, true) => "How Xenon labels and opens this file.",
            (true, false) => "Changes are saved when you connect.",
            (false, false) => "Paste a connection URL, or fill in the fields.",
        };
        let engine_picker = widgets::segmented(
            &theme,
            "engine",
            ServerEngine::ALL
                .into_iter()
                .map(|e| {
                    let label = match e {
                        ServerEngine::Postgres => "Postgres",
                        ServerEngine::Mysql => "MySQL / MariaDB",
                        ServerEngine::SqlServer => "SQL Server",
                    };
                    (label, e == self.engine)
                })
                .collect(),
            cx.listener(|this, ix: &usize, window, cx| {
                this.set_engine(ServerEngine::ALL[*ix], window, cx)
            }),
        );

        let header = div()
            .flex()
            .items_center()
            .gap(px(12.))
            .child(widgets::engine_badge(
                &theme,
                if is_sqlite {
                    Engine::Sqlite
                } else {
                    Engine::from_server(self.engine)
                },
                true,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(crate::theme::text(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(crate::theme::text(11.5))
                            .text_color(theme.t3)
                            .child(subtitle),
                    ),
            );

        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .p(px(20.))
            .child(header);

        if let Some(path) = &self.sqlite_path {
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(widgets::field_label(&theme, "File"))
                    .child(
                        div()
                            .px(px(10.))
                            .py(px(8.))
                            .rounded(px(RADIUS_CONTROL))
                            .bg(theme.s3)
                            .font_family(crate::theme::mono_font())
                            .text_size(crate::theme::text(11.5))
                            .text_color(theme.t2)
                            .child(crate::detect::tilde(path)),
                    ),
            );
        } else {
            if !editing {
                body = body
                    .child(engine_picker)
                    .child(labelled("Connection URL", &self.url, url_focused))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .text_color(theme.t3)
                            .text_size(crate::theme::text(11.))
                            .child(div().flex_1().h(px(1.)).bg(theme.line_2))
                            .child("or")
                            .child(div().flex_1().h(px(1.)).bg(theme.line_2)),
                    );
            }
            body = body
                .child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .child(labelled("Host", &self.host, host_focused))
                        .child(
                            labelled("Port", &self.port, port_focused)
                                .w(px(92.))
                                .flex_none(),
                        ),
                )
                .child(labelled("Database", &self.database, database_focused))
                .child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .child(labelled("User", &self.user, user_focused))
                        .child(labelled("Password", &self.password, password_focused)),
                )
                .child(section(&theme, "SSL mode", ssl_picker))
                // Certificates, for a private CA or a server that wants a client one.
                .when(self.ssl != SslMode::Disable, |el| {
                    let sql_server = self.engine == ServerEngine::SqlServer;
                    el.child(
                        div()
                            .flex()
                            .gap(px(10.))
                            .child(
                                labelled("CA certificate", &self.tls_ca, tls_focus[0])
                                    .flex_1(),
                            )
                            .when(!sql_server, |el| {
                                el.child(
                                    labelled("Client certificate", &self.tls_cert, tls_focus[1])
                                        .flex_1(),
                                )
                                .child(
                                    labelled("Client key", &self.tls_key, tls_focus[2])
                                        .flex_1(),
                                )
                            }),
                    )
                    .child(
                        div()
                            .mt(px(-4.))
                            .text_size(crate::theme::text(11.5))
                            .text_color(theme.t3)
                            .child(if sql_server {
                                "Optional, a PEM file: a private CA to trust. (SQL Server doesn't \
                                 log in with client certificates.)"
                            } else {
                                "Optional, PEM files: a private CA to trust, and a client \
                                 certificate for servers that ask for one."
                            }),
                    )
                })
                .when(
                    (self.environment == Environment::Production || self.remote(cx))
                        && !matches!(self.ssl, SslMode::VerifyFull | SslMode::VerifyCa),
                    |el| {
                        el.child(
                            div()
                                .mt(px(-4.))
                                .text_size(crate::theme::text(11.5))
                                .text_color(theme.warn)
                                .child(if self.environment == Environment::Production {
                                    "Production should verify the server (verify-full). Without it, \
                                     anything between you and the server could pose as it, get the \
                                     password, and read or change what's sent."
                                } else {
                                    "Without verify-full, anything between you and this server could \
                                     pose as it and get the password."
                                }),
                        )
                    },
                )
                .child(
                    div()
                        .id("ssh-toggle")
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .cursor_pointer()
                        .text_color(if self.ssh { theme.text } else { theme.t2 })
                        .hover(|s| s.text_color(theme.text))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.ssh = !this.ssh;
                            cx.notify();
                        }))
                        .child(widgets::checkbox(&theme, self.ssh))
                        .child("Connect through an SSH tunnel")
                        .child(
                            div()
                                .text_size(crate::theme::text(11.5))
                                .text_color(theme.t3)
                                .child("for databases behind a bastion"),
                        ),
                );
            if self.ssh {
                body = body.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .p(px(14.))
                        .rounded(px(widgets::RADIUS_CARD))
                        .bg(theme.s1)
                        .border_1()
                        .border_color(theme.line_2)
                        .child(
                            div()
                                .flex()
                                .gap(px(10.))
                                .child(labelled("SSH host", &self.ssh_host, ssh_focus[0]))
                                .child(
                                    labelled("Port", &self.ssh_port, ssh_focus[1])
                                        .w(px(92.))
                                        .flex_none(),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .gap(px(10.))
                                .child(labelled("User", &self.ssh_user, ssh_focus[2]))
                                .child(labelled("Key file", &self.ssh_key, ssh_focus[3])),
                        )
                        .child(labelled(
                            "Password or key passphrase",
                            &self.ssh_password,
                            ssh_focus[4],
                        ))
                        .child(
                            div()
                                .text_size(crate::theme::text(11.5))
                                .line_height(px(17.))
                                .text_color(theme.t3)
                                .child(
                                    "Anything left empty comes from ~/.ssh/config and your SSH agent. \
                                     The database host and port above are as seen from the SSH server.",
                                ),
                        ),
                );
            }
        }

        // How Xenon treats the connection, set apart from where it points.
        body = body
            .child(div().h(px(1.)).my(px(2.)).bg(theme.line_2))
            .child(labelled("Name", &self.name, name_focused));
        let swatch = |id: usize, color: Option<gpui::Rgba>, on: bool, name: &'static str| {
            div()
                .id(("color", id))
                .size(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_2()
                .border_color(if on {
                    theme.text
                } else {
                    gpui::transparent_black().into()
                })
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.color = name.to_string();
                    cx.notify();
                }))
                .child(match color {
                    Some(color) => div().size(px(14.)).rounded_full().bg(color),
                    // None: an empty ring.
                    None => div()
                        .size(px(14.))
                        .rounded_full()
                        .border_1()
                        .border_color(theme.t3),
                })
        };
        let colors = div()
            .flex()
            .gap(px(4.))
            .child(swatch(0, None, self.color.is_empty(), ""))
            .children(
                widgets::CONNECTION_COLORS
                    .iter()
                    .enumerate()
                    .map(|(ix, (name, _))| {
                        swatch(
                            ix + 1,
                            widgets::connection_color(name),
                            self.color == *name,
                            name,
                        )
                    }),
            );
        body = body.child(section(&theme, "Colour", colors));
        body = body.child(
            div()
                .flex()
                .gap(px(10.))
                .child(section(&theme, "Environment", environment_picker).flex_1())
                .child(
                    section(&theme, "Access", access_picker)
                        .w(px(176.))
                        .flex_none(),
                ),
        );

        body = match &self.status {
            Status::Idle => body,
            Status::Busy(text) => body.child(note(&theme, (*text).into(), theme.t3, None)),
            Status::Failed(error) => body.child(note(
                &theme,
                error.clone(),
                theme.err,
                Some("triangle-alert"),
            )),
            Status::Passed(message) => {
                body.child(note(&theme, message.clone(), theme.ok, Some("check")))
            }
        };

        let save_toggle = div()
            .id("save-toggle")
            .flex()
            .items_center()
            .gap(px(8.))
            .cursor_pointer()
            .text_color(theme.t2)
            .hover(|s| s.text_color(theme.text))
            .on_click(cx.listener(|this, _, _, cx| {
                this.save = !this.save;
                cx.notify();
            }))
            .child(widgets::checkbox(&theme, self.save))
            .child("Save connection");

        let busy = matches!(self.status, Status::Busy(_));
        let footer = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(20.))
            .py(px(14.))
            .border_t_1()
            .border_color(theme.line_2)
            .when(!editing && !is_sqlite, |el| el.child(save_toggle))
            .child(div().flex_1())
            .child(
                widgets::button(&theme, "cancel", "Cancel", ButtonKind::Ghost, true)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ConnectEvent::Dismiss))),
            )
            .when(!is_sqlite, |el| {
                el.child(
                    widgets::button(&theme, "test", "Test", ButtonKind::Secondary, !busy)
                        .on_click(cx.listener(|this, _, _, cx| this.test(cx))),
                )
            })
            .child(
                widgets::button(
                    &theme,
                    "connect",
                    if editing { "Save & connect" } else { "Connect" },
                    ButtonKind::Primary,
                    !busy,
                )
                .child(widgets::kbd_on_accent(&theme, "↵"))
                .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
            );

        // Scroll the fields when the window is too short for them all.
        let room = f32::from(window.viewport_size().height) - 160.;
        let body = body
            .id("connect-body")
            .overflow_y_scroll()
            .max_h(px(room.max(240.)));
        let panel = widgets::overlay_panel(&theme)
            .id("connect-panel")
            .w(px(520.))
            // Clicking anywhere outside the panel closes the dialog.
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(ConnectEvent::Dismiss)))
            .child(body)
            .child(footer);

        div()
            .id("connect-scrim")
            .key_context("ConnectDialog")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &Escape, _, cx| cx.emit(ConnectEvent::Dismiss)))
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.scrim)
            .child(panel)
    }
}

fn section(theme: &Theme, label: &'static str, content: impl IntoElement) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(widgets::field_label(theme, label))
        .child(content)
}

fn note(
    theme: &Theme,
    text: SharedString,
    color: gpui::Rgba,
    icon: Option<&'static str>,
) -> impl IntoElement {
    div()
        .flex()
        .gap(px(9.))
        .px(px(12.))
        .py(px(10.))
        .rounded(px(RADIUS_CONTROL))
        .bg(gpui::Rgba { a: 0.10, ..color })
        .border_1()
        .border_color(gpui::Rgba { a: 0.28, ..color })
        .when_some(icon, |el, icon| {
            el.child(
                gpui::svg()
                    .path(crate::assets::icon(icon))
                    .mt(px(1.))
                    .size(px(14.))
                    .flex_none()
                    .text_color(color),
            )
        })
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_size(crate::theme::text(11.5))
                .line_height(px(17.))
                .text_color(theme.text)
                .child(text),
        )
}
