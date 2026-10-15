//! The connections home: what the main area shows when nothing is connected.

use gpui::{AnyElement, Context, FontWeight, IntoElement, SharedString, div, prelude::*, px, svg};
use xenon_store::{Environment, SavedConnection, ServerEngine, Target};

use crate::assets;
use crate::detect::tilde;
use crate::discover::{Found, FoundTarget};
use crate::servers;
use crate::theme::{MONO_FONT, Theme};
use crate::widgets::{
    ButtonKind, Engine, RADIUS_CARD, RADIUS_CONTROL, button, connection_tags, engine_badge,
    env_tag, field_sized,
};
use crate::workspace::Workspace;
use crate::{NewConnection, OpenFile};

const COLUMN_WIDTH: f32 = 760.;
const GAP: f32 = 10.;
/// Two cards to a row, filling the column.
const CARD_WIDTH: f32 = (COLUMN_WIDTH - GAP) / 2.;

impl Workspace {
    pub(crate) fn render_launcher(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut column = div()
            .w(px(COLUMN_WIDTH))
            .max_w_full()
            .flex()
            .flex_col()
            .gap(px(32.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(
                                div()
                                    .text_size(px(24.))
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
                    .children(self.render_status(theme, cx)),
            )
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
            .pt(px(48.))
            .pb(px(64.))
            .child(column)
            .into_any_element()
    }

    /// "Connecting to …" while a connection opens, or why the last one didn't.
    fn render_status(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if let Some(name) = &self.connecting {
            return Some(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(12.))
                    .text_color(theme.t2)
                    .child(div().size(px(7.)).rounded_full().bg(theme.acc))
                    .child(format!("Connecting to {name}…"))
                    .into_any_element(),
            );
        }
        let notice = self.notice.clone()?;
        Some(
            div()
                .flex()
                .gap(px(10.))
                .px(px(14.))
                .py(px(12.))
                .rounded(px(RADIUS_CARD))
                .bg(gpui::Rgba {
                    a: 0.08,
                    ..theme.err
                })
                .border_1()
                .border_color(gpui::Rgba {
                    a: 0.3,
                    ..theme.err
                })
                .child(
                    svg()
                        .path(assets::icon("triangle-alert"))
                        .mt(px(1.))
                        .size(px(15.))
                        .flex_none()
                        .text_color(theme.err),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child("Couldn't connect"),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .line_height(px(18.))
                                .text_color(theme.t2)
                                .child(notice),
                        ),
                )
                .child(
                    div()
                        .id("dismiss-notice")
                        .size(px(22.))
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .rounded(px(RADIUS_CONTROL))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.s3))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.notice = None;
                            cx.notify();
                        }))
                        .child(
                            svg()
                                .path(assets::icon("x"))
                                .size(px(12.))
                                .text_color(theme.t2),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_paste(&self, theme: &Theme) -> impl IntoElement {
        let focused = self.field_focus.paste;
        let (note, is_error) = &self.paste_note;
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                field_sized(
                    theme,
                    Some(
                        svg()
                            .path(assets::icon("plus"))
                            .size(px(16.))
                            .flex_none()
                            .text_color(if focused { theme.acc } else { theme.t3 })
                            .into_any_element(),
                    ),
                    &self.paste_input,
                    focused,
                    13.,
                )
                .h(px(42.))
                .pl(px(14.))
                .rounded(px(RADIUS_CARD)),
            )
            .when(!note.is_empty(), |el| {
                el.child(
                    div()
                        .pl(px(2.))
                        .text_size(px(11.5))
                        .text_color(if *is_error { theme.err } else { theme.t3 })
                        .child(note.clone()),
                )
            })
    }

    fn render_tiles(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let tile = |id: &'static str,
                    name: &'static str,
                    engine: Engine,
                    kind: &'static str,
                    available: bool| {
            div()
                .id(id)
                .flex_1()
                .min_w(px(0.))
                .h(px(96.))
                .p(px(12.))
                .flex()
                .flex_col()
                .justify_between()
                .rounded(px(RADIUS_CARD))
                .bg(theme.s2)
                .border_1()
                .border_color(theme.line)
                .when(available, |el| {
                    el.cursor_pointer()
                        .hover(|s| s.border_color(theme.acc).bg(theme.s3))
                })
                .child(engine_badge(theme, engine, available))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(if available { theme.text } else { theme.t3 })
                                .child(name),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme.t3)
                                .child(if available { kind } else { "Coming soon" }),
                        ),
                )
        };

        div()
            .flex()
            .gap(px(GAP))
            .child(
                tile(
                    "tile-postgres",
                    "Postgres",
                    Engine::Postgres,
                    "Server",
                    true,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.new_connection(&NewConnection, window, cx)
                })),
            )
            .child(
                tile("tile-sqlite", "SQLite", Engine::Sqlite, "Open a file", true).on_click(
                    cx.listener(|this, _, window, cx| this.open_file(&OpenFile, window, cx)),
                ),
            )
            .child(
                tile(
                    "tile-mysql",
                    "MySQL",
                    Engine::MySql,
                    "MySQL and MariaDB",
                    true,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.new_server_connection(ServerEngine::Mysql, window, cx)
                })),
            )
            .child(
                tile(
                    "tile-duckdb",
                    "DuckDB",
                    Engine::DuckDb,
                    "And CSV, Parquet, JSON",
                    true,
                )
                .on_click(cx.listener(|this, _, window, cx| this.open_file(&OpenFile, window, cx))),
            )
            .child(
                tile(
                    "tile-sqlserver",
                    "SQL Server",
                    Engine::SqlServer,
                    "And Azure SQL",
                    true,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.new_server_connection(ServerEngine::SqlServer, window, cx)
                })),
            )
            .into_any_element()
    }

    fn render_saved_cards(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let groups = [
            (Environment::Production, "Production", theme.err),
            (Environment::Development, "Development", theme.t2),
            (Environment::None, "Other", theme.t3),
        ];
        let mut list = div().flex().flex_col().gap(px(16.));
        for (environment, label, color) in groups {
            let cards: Vec<&SavedConnection> = self
                .saved
                .iter()
                .filter(|s| s.environment == environment)
                .collect();
            if cards.is_empty() {
                continue;
            }
            let mut grid = div().flex().flex_wrap().gap(px(GAP));
            for saved in cards {
                grid = grid.child(self.render_card(saved, theme, cx));
            }
            list = list.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(7.))
                            .text_size(px(11.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.t2)
                            .child(div().size(px(6.)).rounded_full().bg(color))
                            .child(label),
                    )
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
            Target::Sqlite { path } => (crate::widgets::file_engine(path), tilde(path), true),
            Target::Server {
                engine,
                host,
                port,
                database,
                user,
                ssh,
                ..
            } => (
                Engine::from_server(*engine),
                match ssh {
                    Some(tunnel) => format!(
                        "{} via {}",
                        servers::describe(*engine, host, *port, database, user),
                        tunnel.host
                    ),
                    None => servers::describe(*engine, host, *port, database, user),
                },
                matches!(host.as_str(), "" | "localhost" | "127.0.0.1" | "::1"),
            ),
        };
        let engine = self
            .sessions
            .iter()
            .find(|s| s.saved_id == Some(saved.id))
            .map_or(engine, |s| s.engine);
        let group = SharedString::from(format!("card-{}", saved.id));
        let open = self.sessions.iter().any(|s| s.saved_id == Some(saved.id));
        let (connect, edit, forget) = (saved.clone(), saved.clone(), saved.clone());
        let icon_button = |id: (&'static str, usize), icon: &'static str, danger: bool| {
            div()
                .id(id)
                .size(px(26.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(RADIUS_CONTROL))
                .text_color(theme.t3)
                .hover(|s| {
                    s.bg(theme.raised)
                        .text_color(if danger { theme.err } else { theme.text })
                })
                .child(
                    svg()
                        .path(assets::icon(icon))
                        .size(px(14.))
                        .text_color(if danger { theme.err } else { theme.t2 }),
                )
        };

        div()
            .id(group.clone())
            .group(group.clone())
            .w(px(CARD_WIDTH))
            .h(px(64.))
            .pl(px(12.))
            .pr(px(8.))
            .flex()
            .items_center()
            .gap(px(12.))
            .rounded(px(RADIUS_CARD))
            .bg(theme.s2)
            .border_1()
            .border_color(theme.line)
            .cursor_pointer()
            .hover(|s| s.border_color(theme.acc).bg(theme.s3))
            .on_click(
                cx.listener(move |this, _, window, cx| this.connect_to_saved(&connect, window, cx)),
            )
            .child(engine_badge(theme, engine, true))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(3.))
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
                            )
                            .when(
                                matches!(&saved.target, Target::Server { ssh: Some(_), .. }),
                                |el| el.child(env_tag("SSH", theme.t2)),
                            )
                            .when(open, |el| el.child(env_tag("OPEN", theme.ok))),
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
                    .flex_none()
                    .gap(px(2.))
                    .opacity(0.)
                    .group_hover(group, |s| s.opacity(1.))
                    .child(
                        icon_button(("card-edit", saved.id as usize), "pencil", false).on_click(
                            cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.edit_saved(&edit, window, cx);
                            }),
                        ),
                    )
                    .child(
                        icon_button(("card-forget", saved.id as usize), "trash-2", true).on_click(
                            cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.forget_saved(&forget, window, cx);
                            }),
                        ),
                    ),
            )
    }

    fn render_discovery_actions(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let action = |id: &'static str, icon: &'static str, label: &'static str| {
            div()
                .id(id)
                .h(px(24.))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(RADIUS_CONTROL))
                .text_size(px(11.5))
                .text_color(theme.t2)
                .cursor_pointer()
                .hover(|s| s.bg(theme.s3).text_color(theme.text))
                .child(
                    svg()
                        .path(assets::icon(icon))
                        .size(px(13.))
                        .text_color(theme.t3),
                )
                .child(label)
        };
        div()
            .flex()
            .gap(px(2.))
            .child(
                action("scan-project", "folder-search", "Scan a project…")
                    .on_click(cx.listener(|this, _, _, cx| this.scan_project(cx))),
            )
            .child(
                action(
                    "rediscover",
                    "refresh-cw",
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
                .px(px(16.))
                .py(px(14.))
                .rounded(px(RADIUS_CARD))
                .border_1()
                .border_color(theme.line_2)
                .text_size(px(12.))
                .line_height(px(18.))
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
            .rounded(px(RADIUS_CARD))
            .border_1()
            .border_color(theme.line)
            .bg(theme.s2)
            .overflow_hidden();
        for (ix, found) in found.into_iter().enumerate() {
            let target = found.clone();
            let available = !matches!(found.target, FoundTarget::NotYet(_));
            let (action, kind) = match (&found.target, found.ready()) {
                (FoundTarget::NotYet(_), _) => ("Coming soon", None),
                (_, true) => ("Connect", Some(ButtonKind::Primary)),
                (_, false) => ("Set up…", Some(ButtonKind::Secondary)),
            };
            list = list.child(
                div()
                    .id(("found", ix))
                    .h(px(56.))
                    .pl(px(12.))
                    .pr(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .when(ix > 0, |el| el.border_t_1().border_color(theme.line_2))
                    .when(available, |el| {
                        el.cursor_pointer()
                            .hover(|s| s.bg(theme.s3))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.connect_found(&target, window, cx)
                            }))
                    })
                    .child(engine_badge(
                        theme,
                        Engine::from_name(found.engine()),
                        available,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .child(
                                div()
                                    .truncate()
                                    .font_family(MONO_FONT)
                                    .text_size(px(12.))
                                    .text_color(if available { theme.text } else { theme.t2 })
                                    .child(found.detail()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(11.))
                                    .text_color(theme.t3)
                                    .child(found.source.clone()),
                            ),
                    )
                    .child(match kind {
                        Some(kind) => button(theme, ("found-action", ix), action, kind, true)
                            .h(px(26.))
                            .into_any_element(),
                        None => div()
                            .flex_none()
                            .text_size(px(11.5))
                            .text_color(theme.t3)
                            .child(action)
                            .into_any_element(),
                    }),
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
        .gap(px(12.))
        .child(
            div()
                .h(px(24.))
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
