//! The connection editor: connect to Postgres (paste a URL or fill in the
//! fields), or edit a saved connection's name, environment and read-only flag.

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, FontWeight, IntoElement, Render, SharedString,
    Subscription, Window, div, prelude::*, px, rgba,
};
use gpui_component::input::{Escape, Input, InputEvent, InputState};
use xenon_postgres::{ConnectOptions, SslMode};
use xenon_store::{Environment, SavedConnection, Target};

use crate::theme::{ActiveTheme, MONO_FONT, Theme};

/// What the dialog is for.
#[derive(Clone)]
pub enum Subject {
    /// A new Postgres connection.
    New,
    /// A saved connection, with its password from the Keychain.
    Saved(SavedConnection, String),
    /// A new connection with fields filled in (from discovery or a paste).
    Prefill {
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
    pub save: bool,
    pub target: RequestTarget,
}

#[derive(Clone)]
pub enum RequestTarget {
    Postgres(ConnectOptions),
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
    name: Entity<InputState>,
    url: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    database: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    ssl: SslMode,
    environment: Environment,
    read_only: bool,
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

        let (id, saved_name, sqlite_path, options, environment, read_only) = match &subject {
            Subject::New => (
                0,
                String::new(),
                None,
                ConnectOptions::default(),
                Environment::None,
                false,
            ),
            Subject::Prefill { name, options } => (
                0,
                name.clone(),
                None,
                options.clone(),
                Environment::None,
                false,
            ),
            Subject::Saved(saved, password) => {
                let (path, options) = match &saved.target {
                    Target::Sqlite { path } => (Some(path.clone()), ConnectOptions::default()),
                    Target::Postgres {
                        host,
                        port,
                        database,
                        user,
                        ssl,
                    } => (
                        None,
                        ConnectOptions {
                            host: host.clone(),
                            port: *port,
                            database: database.clone(),
                            user: user.clone(),
                            password: password.clone(),
                            ssl: SslMode::parse(ssl).unwrap_or_default(),
                            read_only: saved.read_only,
                        },
                    ),
                };
                (
                    saved.id,
                    saved.name.clone(),
                    path,
                    options,
                    saved.environment,
                    saved.read_only,
                )
            }
        };

        let name = field("e.g. acme production", saved_name, window, cx);
        let url = field(
            "postgres://user:password@host:5432/database",
            String::new(),
            window,
            cx,
        );
        let host = field("localhost", options.host.clone(), window, cx);
        let port = field(
            "5432",
            options.port.map(|p| p.to_string()).unwrap_or_default(),
            window,
            cx,
        );
        let database = field("postgres", options.database.clone(), window, cx);
        let user = field("postgres", options.user.clone(), window, cx);
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .default_value(options.password.clone())
        });

        let subscriptions = [&name, &url, &host, &port, &database, &user, &password]
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
            name,
            url,
            host,
            port,
            database,
            user,
            password,
            ssl: options.ssl,
            environment,
            read_only,
            save: true,
            status: Status::Idle,
            _subscriptions: subscriptions,
        }
    }

    pub fn set_error(&mut self, error: SharedString, cx: &mut Context<Self>) {
        self.status = Status::Failed(error);
        cx.notify();
    }

    pub fn set_test_passed(&mut self, message: SharedString, cx: &mut Context<Self>) {
        self.status = Status::Passed(message);
        cx.notify();
    }

    fn request(&mut self, cx: &mut Context<Self>) -> Option<Request> {
        let value =
            |input: &Entity<InputState>, cx: &App| input.read(cx).value().trim().to_string();
        let mut name = value(&self.name, cx);

        let target = if let Some(path) = &self.sqlite_path {
            RequestTarget::Sqlite(path.clone())
        } else {
            let url = value(&self.url, cx);
            let mut options = if !url.is_empty() {
                match ConnectOptions::from_url(&url) {
                    Ok(options) => options,
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
                }
            };
            options.read_only = self.read_only;
            if name.is_empty() {
                let host = if options.host.is_empty() {
                    "localhost"
                } else {
                    &options.host
                };
                let database = if options.database.is_empty() {
                    "postgres"
                } else {
                    &options.database
                };
                name = format!("{database}@{host}");
            }
            RequestTarget::Postgres(options)
        };

        Some(Request {
            id: self.id,
            name,
            environment: self.environment,
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let editing = self.id != 0;
        let is_sqlite = self.sqlite_path.is_some();

        let labelled = |label: &'static str, input: &Entity<InputState>| {
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .flex_1()
                .child(field_label(&theme, label))
                .child(Input::new(input))
        };

        let ssl_picker = picker(
            &theme,
            "ssl",
            SslMode::ALL
                .into_iter()
                .map(|m| (m.as_str(), m == self.ssl))
                .collect(),
            cx.listener(|this, ix: &usize, _, cx| {
                this.ssl = SslMode::ALL[*ix];
                cx.notify();
            }),
        );
        let environment_picker = picker(
            &theme,
            "environment",
            Environment::ALL
                .into_iter()
                .map(|e| {
                    let label = match e {
                        Environment::None => "none",
                        Environment::Development => "development",
                        Environment::Production => "production",
                    };
                    (label, e == self.environment)
                })
                .collect(),
            cx.listener(|this, ix: &usize, _, cx| {
                this.environment = Environment::ALL[*ix];
                cx.notify();
            }),
        );
        let access_picker = picker(
            &theme,
            "access",
            vec![
                ("read & write", !self.read_only),
                ("read-only", self.read_only),
            ],
            cx.listener(|this, ix: &usize, _, cx| {
                this.read_only = *ix == 1;
                cx.notify();
            }),
        );

        let title = match (editing, is_sqlite) {
            (_, true) => "Edit connection",
            (true, false) => "Edit Postgres connection",
            (false, false) => "Connect to Postgres",
        };

        let mut panel = div()
            .id("connect-panel")
            .w(px(480.))
            .flex()
            .flex_col()
            .gap(px(14.))
            .p(px(20.))
            .bg(theme.s1)
            .border_1()
            .border_color(theme.line)
            // Clicking anywhere outside the panel closes the dialog.
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(ConnectEvent::Dismiss)))
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(labelled("Name", &self.name));

        if let Some(path) = &self.sqlite_path {
            panel = panel.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(field_label(&theme, "File"))
                    .child(
                        div()
                            .font_family(MONO_FONT)
                            .text_size(px(11.))
                            .text_color(theme.t2)
                            .child(path.clone()),
                    ),
            );
        } else {
            if !editing {
                panel = panel.child(labelled("Connection URL", &self.url)).child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .text_color(theme.t3)
                        .font_family(MONO_FONT)
                        .text_size(px(10.))
                        .child(div().flex_1().h(px(1.)).bg(theme.line))
                        .child("OR")
                        .child(div().flex_1().h(px(1.)).bg(theme.line)),
                );
            }
            panel = panel
                .child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .child(labelled("Host", &self.host))
                        .child(labelled("Port", &self.port).w(px(90.)).flex_none()),
                )
                .child(labelled("Database", &self.database))
                .child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .child(labelled("User", &self.user))
                        .child(labelled("Password", &self.password)),
                )
                .child(section(&theme, "SSL mode", ssl_picker));
        }

        panel = panel
            .child(section(&theme, "Environment", environment_picker))
            .child(section(&theme, "Access", access_picker));

        panel = match &self.status {
            Status::Idle => panel,
            Status::Busy(text) => panel.child(div().text_color(theme.t2).child(*text)),
            Status::Failed(error) => panel.child(note(&theme, error.clone(), theme.err)),
            Status::Passed(message) => panel.child(note(&theme, message.clone(), theme.ok)),
        };

        let save_toggle = div()
            .id("save-toggle")
            .flex()
            .items_center()
            .gap(px(6.))
            .cursor_pointer()
            .text_color(theme.t2)
            .on_click(cx.listener(|this, _, _, cx| {
                this.save = !this.save;
                cx.notify();
            }))
            .child(
                div()
                    .size(px(12.))
                    .border_1()
                    .border_color(if self.save { theme.acc } else { theme.t3 })
                    .when(self.save, |el| el.bg(theme.acc)),
            )
            .child("Save connection");

        let panel = panel.child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .when(!editing && !is_sqlite, |el| el.child(save_toggle))
                .child(div().flex_1())
                .when(!is_sqlite, |el| {
                    el.child(
                        button(&theme, "test", "Test", false)
                            .on_click(cx.listener(|this, _, _, cx| this.test(cx))),
                    )
                })
                .child(
                    button(&theme, "cancel", "Cancel", false)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(ConnectEvent::Dismiss))),
                )
                .child(
                    button(
                        &theme,
                        "connect",
                        if editing { "Save & connect" } else { "Connect" },
                        true,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                ),
        );

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
            .bg(match theme.mode {
                crate::theme::Mode::Dark => rgba(0x00000099),
                crate::theme::Mode::Light => rgba(0x17181b40),
            })
            .child(panel)
    }
}

/// A row of mutually exclusive options; `on_pick` receives the chosen index.
fn picker(
    theme: &Theme,
    id: &'static str,
    options: Vec<(&'static str, bool)>,
    on_pick: impl Fn(&usize, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let on_pick = std::rc::Rc::new(on_pick);
    div()
        .flex()
        .flex_row()
        .border_1()
        .border_color(theme.line)
        .children(
            options
                .into_iter()
                .enumerate()
                .map(|(ix, (label, active))| {
                    let on_pick = on_pick.clone();
                    div()
                        .id((id, ix))
                        .flex_1()
                        .py(px(5.))
                        .flex()
                        .justify_center()
                        .font_family(MONO_FONT)
                        .text_size(px(10.5))
                        .cursor_pointer()
                        .when(active, |el| el.bg(theme.s3).text_color(theme.text))
                        .when(!active, |el| {
                            el.text_color(theme.t3).hover(|s| s.text_color(theme.t2))
                        })
                        .on_click(move |_, window, cx| on_pick(&ix, window, cx))
                        .child(label)
                }),
        )
}

fn section(theme: &Theme, label: &'static str, content: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(field_label(theme, label))
        .child(content)
}

fn note(theme: &Theme, text: SharedString, color: gpui::Rgba) -> impl IntoElement {
    div()
        .p(px(10.))
        .bg(theme.s2)
        .border_l_2()
        .border_color(color)
        .font_family(MONO_FONT)
        .text_size(px(11.))
        .text_color(theme.t2)
        .child(text)
}

fn field_label(theme: &Theme, label: &'static str) -> impl IntoElement {
    div()
        .font_family(MONO_FONT)
        .text_size(px(10.))
        .text_color(theme.t3)
        .child(label.to_uppercase())
}

fn button(
    theme: &Theme,
    id: &'static str,
    label: &'static str,
    primary: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px(px(12.))
        .py(px(6.))
        .border_1()
        .cursor_pointer()
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .when(primary, |el| {
            el.bg(theme.acc)
                .border_color(theme.acc)
                .text_color(theme.bg)
        })
        .when(!primary, |el| {
            el.border_color(theme.line)
                .text_color(theme.t2)
                .hover(|s| s.text_color(theme.text).border_color(theme.t3))
        })
        .child(label)
}
