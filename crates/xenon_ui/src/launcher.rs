//! The connections home: what the main area shows when nothing is connected.

use gpui::{AnyElement, Context, FontWeight, IntoElement, SharedString, div, prelude::*, px, svg};
use xenon_store::{Environment, SavedConnection, Target};

use crate::assets;
use crate::detect::tilde;
use crate::discover::{Found, FoundTarget};
use crate::theme::{MONO_FONT, Theme};
use crate::widgets::{connection_tags, env_tag, field};
use crate::workspace::Workspace;
use crate::{NewConnection, OpenFile};

const COLUMN_WIDTH: f32 = 760.;
const CARD_WIDTH: f32 = 372.;

impl Workspace {
    pub(crate) fn render_launcher(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut column = div()
            .w(px(COLUMN_WIDTH))
            .max_w_full()
            .flex()
            .flex_col()
            .gap(px(28.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .text_size(px(22.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Connect"),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(theme.t2)
                            .child("Paste a connection, open a file, or pick one below."),
                    ),
            )
            .child(self.render_paste(theme))
            .child(section(
                theme,
                "New connection",
                None,
                self.render_tiles(theme, cx),
            ));

        if !self.saved.is_empty() {
            column = column.child(section(
                theme,
                "Saved",
                None,
                self.render_saved_cards(theme, cx),
            ));
        }
        column = column.child(section(
            theme,
            "On this Mac",
            Some(self.render_discovery_actions(theme, cx).into_any_element()),
            self.render_found(theme, cx),
        ));

        div()
            .id("launcher")
            .size_full()
            .overflow_y_scroll()
            .bg(theme.s1)
            .flex()
            .justify_center()
            .px(px(32.))
            .py(px(36.))
            .child(column)
            .into_any_element()
    }

    fn render_paste(&self, theme: &Theme) -> impl IntoElement {
        let focused = self.field_focus.paste;
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                field(
                    theme,
                    svg()
                        .path(assets::icon("plus"))
                        .size(px(16.))
                        .flex_none()
                        .text_color(if focused { theme.acc } else { theme.t3 })
                        .into_any_element(),
                    &self.paste_input,
                    focused,
                )
                .h(px(40.))
                .text_size(px(14.)),
            )
            .child(
                div()
                    .h(px(16.))
                    .font_family(MONO_FONT)
                    .text_size(px(11.))
                    .text_color(if self.paste_note.1 {
                        theme.err
                    } else {
                        theme.t3
                    })
                    .child(self.paste_note.0.clone()),
            )
    }

    fn render_tiles(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let tile = |id: &'static str, name: &'static str, kind: &'static str, available: bool| {
            div()
                .id(id)
                .w(px(140.))
                .h(px(72.))
                .p(px(12.))
                .flex()
                .flex_col()
                .justify_between()
                .rounded(px(8.))
                .bg(theme.s2)
                .border_1()
                .border_color(theme.line)
                .when(available, |el| {
                    el.cursor_pointer()
                        .hover(|s| s.border_color(theme.acc).bg(theme.s3))
                })
                .when(!available, |el| el.opacity(0.5))
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(name),
                )
                .child(
                    div()
                        .font_family(MONO_FONT)
                        .text_size(px(10.))
                        .text_color(if available { theme.t3 } else { theme.warn })
                        .child(if available { kind } else { "soon" }),
                )
        };

        div()
            .flex()
            .flex_wrap()
            .gap(px(10.))
            .child(tile("tile-postgres", "Postgres", "server", true).on_click(
                cx.listener(|this, _, window, cx| this.new_connection(&NewConnection, window, cx)),
            ))
            .child(
                tile("tile-sqlite", "SQLite", "file", true).on_click(
                    cx.listener(|this, _, window, cx| this.open_file(&OpenFile, window, cx)),
                ),
            )
            .child(tile("tile-mysql", "MySQL", "server", false))
            .child(tile("tile-duckdb", "DuckDB", "file", false))
            .child(tile("tile-sqlserver", "SQL Server", "server", false))
            .into_any_element()
    }

    fn render_saved_cards(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let groups = [
            (Environment::Production, "Production"),
            (Environment::Development, "Development"),
            (Environment::None, "Other"),
        ];
        let mut list = div().flex().flex_col().gap(px(14.));
        for (environment, label) in groups {
            let cards: Vec<&SavedConnection> = self
                .saved
                .iter()
                .filter(|s| s.environment == environment)
                .collect();
            if cards.is_empty() {
                continue;
            }
            let mut grid = div().flex().flex_wrap().gap(px(10.));
            for saved in cards {
                grid = grid.child(self.render_card(saved, theme, cx));
            }
            list = list.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(div().text_size(px(11.)).text_color(theme.t3).child(label))
                    .child(grid),
            );
        }
        list.into_any_element()
    }

    fn render_card(
        &self,
        saved: &SavedConnection,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (engine, detail, local) = match &saved.target {
            Target::Sqlite { path } => ("SQ", tilde(path), true),
            Target::Postgres {
                host,
                port,
                database,
                user,
                ..
            } => (
                "PG",
                format!("{user}@{host}:{}/{database}", port.unwrap_or(5432)),
                matches!(host.as_str(), "" | "localhost" | "127.0.0.1" | "::1"),
            ),
        };
        let group = SharedString::from(format!("card-{}", saved.id));
        let (connect, edit, forget) = (saved.clone(), saved.clone(), saved.clone());

        div()
            .id(group.clone())
            .group(group.clone())
            .w(px(CARD_WIDTH))
            .h(px(64.))
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(12.))
            .rounded(px(8.))
            .bg(theme.s2)
            .border_1()
            .border_color(theme.line)
            .cursor_pointer()
            .hover(|s| s.border_color(theme.acc).bg(theme.s3))
            .on_click(cx.listener(move |this, _, _, cx| this.connect_to_saved(&connect, cx)))
            .child(engine_badge(theme, engine))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(saved.name.clone()),
                            )
                            .children(
                                connection_tags(theme, saved.environment, saved.read_only, local)
                                    .into_iter()
                                    .filter(|(label, _)| *label != "LOCAL")
                                    .map(|(label, color)| env_tag(label, color)),
                            ),
                    )
                    .child(
                        div()
                            .truncate()
                            .font_family(MONO_FONT)
                            .text_size(px(10.5))
                            .text_color(theme.t3)
                            .child(detail),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(10.))
                    .opacity(0.)
                    .group_hover(group, |s| s.opacity(1.))
                    .child(
                        div()
                            .id(("card-edit", saved.id as usize))
                            .text_color(theme.t3)
                            .hover(|s| s.text_color(theme.text))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.edit_saved(&edit, window, cx);
                            }))
                            .child("✎"),
                    )
                    .child(
                        div()
                            .id(("card-forget", saved.id as usize))
                            .text_color(theme.t3)
                            .hover(|s| s.text_color(theme.err))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.forget_saved(&forget, window, cx);
                            }))
                            .child("✕"),
                    ),
            )
    }

    fn render_discovery_actions(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let link = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .text_size(px(11.5))
                .text_color(theme.acc)
                .cursor_pointer()
                .hover(|s| s.text_color(theme.text))
                .child(label)
        };
        div()
            .flex()
            .gap(px(14.))
            .child(
                link("scan-project", "Scan a project's .env files…")
                    .on_click(cx.listener(|this, _, _, cx| this.scan_project(cx))),
            )
            .child(
                link(
                    "rediscover",
                    if self.discovering {
                        "Looking…"
                    } else {
                        "Refresh"
                    },
                )
                .on_click(cx.listener(|this, _, _, cx| this.rediscover(cx))),
            )
    }

    fn render_found(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let found: Vec<&Found> = self.project_found.iter().chain(&self.found).collect();
        if found.is_empty() {
            return div()
                .text_size(px(12.5))
                .text_color(theme.t3)
                .child(if self.discovering {
                    "Looking for databases on this Mac…"
                } else {
                    "Nothing found. Xenon looks at Docker containers, servers on the usual ports, ~/.pgpass and ~/.pg_service.conf."
                })
                .into_any_element();
        }

        let mut list = div()
            .flex()
            .flex_col()
            .rounded(px(8.))
            .border_1()
            .border_color(theme.line)
            .overflow_hidden();
        for (ix, found) in found.into_iter().enumerate() {
            let target = found.clone();
            let available = !matches!(found.target, FoundTarget::NotYet(_));
            let action = match (&found.target, found.ready()) {
                (FoundTarget::NotYet(_), _) => "Soon",
                (_, true) => "Connect",
                (_, false) => "Set up…",
            };
            list = list.child(
                div()
                    .id(("found", ix))
                    .h(px(52.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .bg(theme.s2)
                    .when(ix > 0, |el| el.border_t_1().border_color(theme.line_2))
                    .when(available, |el| {
                        el.cursor_pointer()
                            .hover(|s| s.bg(theme.s3))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.connect_found(&target, window, cx)
                            }))
                    })
                    .when(!available, |el| el.opacity(0.55))
                    .child(engine_badge(
                        theme,
                        match found.engine() {
                            "Postgres" => "PG",
                            "MySQL" => "MY",
                            "SQL Server" => "MS",
                            "DuckDB" => "DK",
                            _ => "··",
                        },
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(div().truncate().text_size(px(13.)).child(found.detail()))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(11.))
                                    .text_color(theme.t3)
                                    .child(found.source.clone()),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if available { theme.acc } else { theme.warn })
                            .child(action),
                    ),
            );
        }
        list.into_any_element()
    }
}

fn section(
    theme: &Theme,
    title: &'static str,
    trailing: Option<AnyElement>,
    content: AnyElement,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .font_family(MONO_FONT)
                        .text_size(px(10.5))
                        .text_color(theme.t3)
                        .child(title.to_uppercase()),
                )
                .children(trailing),
        )
        .child(content)
}

fn engine_badge(theme: &Theme, label: &'static str) -> impl IntoElement {
    div()
        .size(px(30.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .bg(theme.acc_wash)
        .font_family(MONO_FONT)
        .text_size(px(10.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.acc)
        .child(label)
}
