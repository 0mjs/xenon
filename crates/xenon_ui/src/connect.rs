//! "Connect to Postgres" dialog: paste a URL, or fill in the fields.

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, FontWeight, IntoElement, Render,
    SharedString, Subscription, Window, div, prelude::*, px, rgba,
};
use gpui_component::input::{Escape, Input, InputEvent, InputState};
use xenon_postgres::{ConnectOptions, SslMode};

use crate::theme::{ActiveTheme, MONO_FONT, Theme};

pub enum PgTarget {
    Url(String),
    Options(ConnectOptions),
}

pub enum ConnectEvent {
    Submit(PgTarget),
    Dismiss,
}

pub struct ConnectDialog {
    focus_handle: FocusHandle,
    url: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    database: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    ssl: SslMode,
    error: Option<SharedString>,
    connecting: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ConnectEvent> for ConnectDialog {}

impl ConnectDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let field = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let url = field("postgres://user:password@host:5432/database", window, cx);
        let host = field("localhost", window, cx);
        let port = field("5432", window, cx);
        let database = field("postgres", window, cx);
        let user = field("postgres", window, cx);
        let password = cx.new(|cx| InputState::new(window, cx).masked(true));

        let subscriptions = [&url, &host, &port, &database, &user, &password]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this, _, event: &InputEvent, _, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.submit(cx);
                    }
                })
            })
            .collect();

        url.update(cx, |input, cx| input.focus(window, cx));

        Self {
            focus_handle: cx.focus_handle(),
            url,
            host,
            port,
            database,
            user,
            password,
            ssl: SslMode::Prefer,
            error: None,
            connecting: false,
            _subscriptions: subscriptions,
        }
    }

    pub fn set_error(&mut self, error: SharedString, cx: &mut Context<Self>) {
        self.error = Some(error);
        self.connecting = false;
        cx.notify();
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.connecting {
            return;
        }
        let value = |input: &Entity<InputState>, cx: &App| input.read(cx).value().trim().to_string();

        let url = value(&self.url, cx);
        let target = if !url.is_empty() {
            PgTarget::Url(url)
        } else {
            let port = value(&self.port, cx);
            let port = if port.is_empty() {
                None
            } else {
                match port.parse() {
                    Ok(port) => Some(port),
                    Err(_) => {
                        self.error = Some(format!("\"{port}\" is not a valid port.").into());
                        cx.notify();
                        return;
                    }
                }
            };
            PgTarget::Options(ConnectOptions {
                host: value(&self.host, cx),
                port,
                database: value(&self.database, cx),
                user: value(&self.user, cx),
                password: self.password.read(cx).value().to_string(),
                ssl: self.ssl,
            })
        };

        self.error = None;
        self.connecting = true;
        cx.emit(ConnectEvent::Submit(target));
        cx.notify();
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(ConnectEvent::Dismiss);
    }
}

impl Render for ConnectDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();

        let labelled = |label: &'static str, input: &Entity<InputState>| {
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .flex_1()
                .child(field_label(&theme, label))
                .child(Input::new(input))
        };

        let ssl_options = div().flex().flex_row().border_1().border_color(theme.line).children(
            SslMode::ALL.into_iter().map(|mode| {
                let active = mode == self.ssl;
                div()
                    .id(mode.as_str())
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
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.ssl = mode;
                        cx.notify();
                    }))
                    .child(mode.as_str())
            }),
        );

        let panel = div()
            .id("connect-panel")
            .w(px(460.))
            .flex()
            .flex_col()
            .gap(px(14.))
            .p(px(20.))
            .bg(theme.s1)
            .border_1()
            .border_color(theme.line)
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Connect to Postgres"),
            )
            .child(labelled("Connection URL", &self.url))
            .child(
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
            )
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
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(field_label(&theme, "SSL mode"))
                    .child(ssl_options),
            )
            .when_some(self.error.clone(), |el, error| {
                el.child(
                    div()
                        .p(px(10.))
                        .bg(theme.s2)
                        .border_l_2()
                        .border_color(theme.err)
                        .font_family(MONO_FONT)
                        .text_size(px(11.))
                        .text_color(theme.t2)
                        .child(error),
                )
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        button(&theme, "cancel", "Cancel", false)
                            .on_click(cx.listener(|this, _, _, cx| this.dismiss(cx))),
                    )
                    .child(
                        button(
                            &theme,
                            "connect",
                            if self.connecting { "Connecting…" } else { "Connect" },
                            true,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                    ),
            );

        div()
            .id("connect-scrim")
            .key_context("ConnectDialog")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Escape, _, cx| this.dismiss(cx)))
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
            .on_click(cx.listener(|this, _, _, cx| this.dismiss(cx)))
            .child(panel)
    }
}

fn field_label(theme: &Theme, label: &'static str) -> impl IntoElement {
    div()
        .font_family(MONO_FONT)
        .text_size(px(10.))
        .text_color(theme.t3)
        .child(label.to_uppercase())
}

fn button(theme: &Theme, id: &'static str, label: &'static str, primary: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px(px(12.))
        .py(px(6.))
        .border_1()
        .cursor_pointer()
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .when(primary, |el| {
            el.bg(theme.text).border_color(theme.text).text_color(theme.bg)
        })
        .when(!primary, |el| {
            el.border_color(theme.line)
                .text_color(theme.t2)
                .hover(|s| s.text_color(theme.text).border_color(theme.t3))
        })
        .child(label)
}
