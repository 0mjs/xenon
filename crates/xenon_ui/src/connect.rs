//! The connection editor: connect to Postgres (paste a URL or fill in the
//! fields), or edit a saved connection's name, environment and read-only flag.

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, FontWeight, IntoElement, Render, SharedString,
    Subscription, Window, div, prelude::*, px,
};
use gpui_component::input::{Escape, InputEvent, InputState};
use xenon_postgres::{ConnectOptions, SslMode};
use xenon_store::{Environment, SavedConnection, Target};

use crate::theme::{ActiveTheme, MONO_FONT, Theme};
use crate::widgets::{self, ButtonKind, RADIUS_CONTROL};

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

        let ssl_picker = widgets::segmented(
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
                cx.notify();
            }),
        );
        let access_picker = widgets::segmented(
            &theme,
            "access",
            vec![("Read & write", !self.read_only), ("Read-only", self.read_only)],
            cx.listener(|this, ix: &usize, _, cx| {
                this.read_only = *ix == 1;
                cx.notify();
            }),
        );

        let (title, subtitle) = match (editing, is_sqlite) {
            (_, true) => ("Edit connection", "How Xenon labels and opens this file."),
            (true, false) => ("Edit connection", "Changes are saved when you connect."),
            (false, false) => (
                "Connect to Postgres",
                "Paste a connection URL, or fill in the fields.",
            ),
        };

        let header = div()
            .flex()
            .items_center()
            .gap(px(12.))
            .child(engine_badge(&theme, if is_sqlite { "SQ" } else { "PG" }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(div().text_size(px(11.5)).text_color(theme.t3).child(subtitle)),
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
                            .font_family(MONO_FONT)
                            .text_size(px(11.5))
                            .text_color(theme.t2)
                            .child(crate::detect::tilde(path)),
                    ),
            );
        } else {
            if !editing {
                body = body.child(labelled("Connection URL", &self.url, url_focused)).child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .text_color(theme.t3)
                        .text_size(px(11.))
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
                .child(section(&theme, "SSL mode", ssl_picker));
        }

        // How Xenon treats the connection, set apart from where it points.
        body = body.child(div().h(px(1.)).my(px(2.)).bg(theme.line_2)).child(
            labelled("Name", &self.name, name_focused),
        );
        body = body.child(
            div()
                .flex()
                .gap(px(10.))
                .child(section(&theme, "Environment", environment_picker).flex_1())
                .child(section(&theme, "Access", access_picker).w(px(176.)).flex_none()),
        );

        body = match &self.status {
            Status::Idle => body,
            Status::Busy(text) => body.child(note(&theme, (*text).into(), theme.t3, None)),
            Status::Failed(error) => {
                body.child(note(&theme, error.clone(), theme.err, Some("triangle-alert")))
            }
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

/// The two-letter engine mark used on cards and dialogs.
pub fn engine_badge(theme: &Theme, label: &'static str) -> impl IntoElement {
    div()
        .size(px(32.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(RADIUS_CONTROL + 1.))
        .bg(theme.acc_wash)
        .font_family(MONO_FONT)
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.acc)
        .child(label)
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
                .text_size(px(11.5))
                .line_height(px(17.))
                .text_color(theme.text)
                .child(text),
        )
}
