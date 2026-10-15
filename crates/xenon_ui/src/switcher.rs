//! The status bar's connection menu: switch to an open connection, open a
//! saved one, or start a new one.

use gpui::{AnyElement, Context, SharedString, div, prelude::*, px};

use crate::theme::Theme;
use crate::widgets::{self, Engine, connection_color, engine_logo};
use crate::workspace::Workspace;

impl Workspace {
    pub(crate) fn toggle_switcher(&mut self, cx: &mut Context<Self>) {
        self.switcher = !self.switcher;
        cx.notify();
    }

    pub(crate) fn render_switcher(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.switcher {
            return None;
        }
        let current = self.active_tab().session;
        let row = |id: SharedString| {
            div()
                .id(id)
                .h(px(28.))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(8.))
                .rounded(px(widgets::RADIUS_CONTROL))
                .cursor_pointer()
                .hover(|s| s.bg(theme.s3))
        };
        let heading = |text: &'static str| {
            div()
                .px(px(8.))
                .pt(px(6.))
                .pb(px(2.))
                .text_size(px(10.))
                .text_color(theme.t3)
                .child(text.to_uppercase())
        };
        let open = self.sessions.iter().map(|session| {
            let id = session.id;
            row(SharedString::from(format!("switch-{id}")))
                .when(current == Some(id), |el| el.bg(theme.acc_wash))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.switcher = false;
                    this.attach(id, true, window, cx);
                }))
                .child(engine_logo(
                    session.engine,
                    13.,
                    connection_color(&session.profile.color).unwrap_or(theme.t2),
                ))
                .child(div().flex_1().truncate().child(session.name.clone()))
        });
        let saved = self
            .saved
            .iter()
            .filter(|saved| !self.sessions.iter().any(|s| s.saved_id == Some(saved.id)))
            .map(|saved| {
                let engine = match &saved.target {
                    xenon_store::Target::Sqlite { path } => widgets::file_engine(path),
                    xenon_store::Target::Server { engine, .. } => Engine::from_server(*engine),
                };
                let target = saved.clone();
                row(SharedString::from(format!("switch-saved-{}", saved.id)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.switcher = false;
                        this.connect_to_saved(&target, window, cx);
                    }))
                    .child(engine_logo(
                        engine,
                        13.,
                        connection_color(&saved.color).unwrap_or(theme.t3),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .text_color(theme.t2)
                            .child(saved.name.clone()),
                    )
            })
            .collect::<Vec<_>>();
        Some(
            gpui::deferred(
                div().absolute().left(px(8.)).bottom(px(30.)).child(
                    widgets::overlay_panel(theme)
                        .id("switcher")
                        .w(px(280.))
                        .max_h(px(420.))
                        .overflow_y_scroll()
                        .h_auto()
                        .p(px(4.))
                        .text_size(px(12.))
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.switcher = false;
                            cx.notify();
                        }))
                        .when(!self.sessions.is_empty(), |el| {
                            el.child(heading("Open")).children(open)
                        })
                        .when(!saved.is_empty(), |el| {
                            el.child(heading("Saved")).children(saved)
                        })
                        .child(div().my(px(4.)).h(px(1.)).bg(theme.line_2))
                        .child(
                            row("switch-new".into())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.switcher = false;
                                    this.open_connect_tab(window, cx);
                                }))
                                .child(
                                    gpui::svg()
                                        .path(crate::assets::icon("plus"))
                                        .size(px(13.))
                                        .text_color(theme.t2),
                                )
                                .child("New connection…"),
                        ),
                ),
            )
            .into_any_element(),
        )
    }
}
