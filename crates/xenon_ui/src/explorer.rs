//! The explorer panel: connection, schemas, tables and their columns.

use gpui::{AnyElement, Context, FontWeight, IntoElement, SharedString, div, prelude::*, px, svg};
use xenon_driver::{ObjectKind, SchemaObject};
use xenon_store::{SavedConnection, Target};

use crate::assets;
use crate::palette::fuzzy_score;
use crate::theme::{MONO_FONT, Theme};
use crate::widgets::{
    Engine, connection_tags, empty_hint, engine_logo, env_tag, panel_label, search_field, tree_row,
};
use crate::workspace::{Session, Workspace};

impl Workspace {
    pub(crate) fn render_explorer(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut list = div()
            .id("explorer-list")
            .flex_1()
            .overflow_y_scroll()
            .pb(px(8.));

        if self.sessions.is_empty() {
            list = list.child(empty_hint(theme, "Not connected"));
        }
        let filter = self.explorer_filter.read(cx).value().trim().to_string();
        let active = self.active_tab().session;
        let mut shown = 0;
        for session in &self.sessions {
            let (rows, matches) =
                self.render_session(session, active == Some(session.id), &filter, theme, cx);
            shown += matches;
            list = list.children(rows);
        }
        if !filter.is_empty() && shown == 0 && !self.sessions.is_empty() {
            list = list.child(empty_hint(theme, "No matching tables"));
        }

        if let Some(notice) = &self.notice {
            list = list.child(
                div()
                    .mx(px(12.))
                    .mt(px(10.))
                    .p(px(8.))
                    .border_l_2()
                    .border_color(theme.err)
                    .bg(theme.s2)
                    .text_color(theme.t2)
                    .child(notice.clone()),
            );
        }

        if !self.saved.is_empty() {
            list = list.child(div().mt(px(8.)).child(panel_label(theme, "Saved")));
            let current = self.active_session().and_then(|s| s.saved_id);
            for saved in &self.saved {
                let open = self.sessions.iter().any(|s| s.saved_id == Some(saved.id));
                let active = current == Some(saved.id);
                list = list.child(self.render_saved(saved, open, active, theme, cx));
            }
        }

        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .when(!self.sessions.is_empty(), |el| {
                el.child(div().px(px(10.)).pb(px(8.)).child(search_field(
                    theme,
                    &self.explorer_filter,
                    self.field_focus.explorer,
                )))
            })
            .child(list)
            .into_any_element()
    }

    /// One open connection: a header that folds it, then its schemas and tables.
    /// Returns the rows and how many tables matched the filter.
    fn render_session(
        &self,
        session: &Session,
        active: bool,
        filter: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> (Vec<AnyElement>, usize) {
        let id = session.id;
        let group = SharedString::from(format!("session-{id}"));
        // A filter shows every match, even in folded connections and schemas.
        let folded = session.collapsed && filter.is_empty();
        let mut rows = vec![
            tree_row(0)
                .id(group.clone())
                .group(group.clone())
                .mt(px(2.))
                .cursor_pointer()
                .hover(|s| s.bg(theme.s3))
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_session(id, cx)))
                .child(div().w(px(12.)).text_color(theme.t3).child(if folded {
                    "▸"
                } else {
                    "▾"
                }))
                .child(engine_logo(
                    session.engine,
                    13.,
                    if active { theme.acc } else { theme.t3 },
                ))
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(if active { theme.text } else { theme.t2 })
                        .child(session.name.clone()),
                )
                .children(
                    connection_tags(
                        theme,
                        session.environment,
                        session.read_only,
                        session.conn.is_local(),
                    )
                    .into_iter()
                    .map(|(label, color)| env_tag(label, color)),
                )
                .when(session.conn.tunnel().is_some(), |el| {
                    el.child(env_tag("SSH", theme.t2))
                })
                .child(
                    div()
                        .id(("close-session", id))
                        .size(px(22.))
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .rounded(px(5.))
                        .opacity(0.6)
                        .group_hover(group, |s| s.opacity(1.))
                        .hover(|s| s.bg(theme.raised))
                        .tooltip(crate::widgets::tooltip("Disconnect", "⇧⌘W"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_session(id, window, cx);
                        }))
                        .child(
                            svg()
                                .path(assets::icon("unplug"))
                                .size(px(13.))
                                .text_color(theme.t2),
                        ),
                )
                .into_any_element(),
        ];
        if folded {
            return (rows, 0);
        }

        let nested = session.shows_schemas();
        let mut shown = 0;
        for schema in &session.schemas {
            let objects: Vec<&SchemaObject> = schema
                .objects
                .iter()
                .filter(|o| filter.is_empty() || fuzzy_score(filter, &o.name).is_some())
                .collect();
            if !filter.is_empty() && objects.is_empty() {
                continue;
            }
            let collapsed =
                nested && filter.is_empty() && session.collapsed_schemas.contains(&schema.name);
            if nested {
                let name = schema.name.clone();
                rows.push(
                    tree_row(1)
                        .id(SharedString::from(format!("schema-{id}-{}", schema.name)))
                        .cursor_pointer()
                        .text_color(theme.t2)
                        .hover(|s| s.bg(theme.s3))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.toggle_schema(id, &name, cx)),
                        )
                        .child(div().w(px(12.)).text_color(theme.t3).child(if collapsed {
                            "▸"
                        } else {
                            "▾"
                        }))
                        .child(div().flex_1().truncate().child(schema.name.clone()))
                        .child(count(theme, objects.len()))
                        .into_any_element(),
                );
            }
            if collapsed {
                continue;
            }
            let depth = if nested { 2 } else { 1 };
            for object in objects {
                shown += 1;
                rows.extend(self.render_object(session, &schema.name, object, depth, theme, cx));
            }
            if filter.is_empty() && schema.objects.is_empty() {
                rows.push(empty_hint(theme, "No tables"));
            }
        }
        (rows, shown)
    }

    fn render_saved(
        &self,
        saved: &SavedConnection,
        open: bool,
        active: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (engine, local) = match &saved.target {
            Target::Sqlite { .. } => (Engine::Sqlite, true),
            Target::Server { engine, host, .. } => (
                Engine::from_server(*engine),
                matches!(host.as_str(), "" | "localhost" | "127.0.0.1" | "::1"),
            ),
        };
        // Once open, the logo can say MariaDB rather than MySQL.
        let engine = self
            .sessions
            .iter()
            .find(|s| s.saved_id == Some(saved.id))
            .map_or(engine, |s| s.engine);
        let group = SharedString::from(format!("saved-{}", saved.id));
        let (connect, edit, forget) = (saved.clone(), saved.clone(), saved.clone());

        tree_row(0)
            .id(group.clone())
            .group(group.clone())
            .cursor_pointer()
            .text_color(if open { theme.text } else { theme.t2 })
            .when(active, |el| el.bg(theme.acc_wash))
            .when(!active, |el| {
                el.hover(|s| s.bg(theme.s3).text_color(theme.text))
            })
            .on_click(
                cx.listener(move |this, _, window, cx| this.connect_to_saved(&connect, window, cx)),
            )
            .child(div().w(px(18.)).flex().flex_none().child(engine_logo(
                engine,
                13.,
                if open { theme.acc } else { theme.t3 },
            )))
            .child(div().flex_1().truncate().child(saved.name.clone()))
            .children(
                connection_tags(theme, saved.environment, saved.read_only, local)
                    .into_iter()
                    .filter(|(label, _)| *label != "LOCAL")
                    .map(|(label, color)| env_tag(label, color)),
            )
            .child(
                div()
                    .flex()
                    .gap(px(6.))
                    .opacity(0.)
                    .group_hover(group, |s| s.opacity(1.))
                    .child(
                        div()
                            .id(("edit-saved", saved.id as usize))
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
                            .id(("forget-saved", saved.id as usize))
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

    fn render_object(
        &self,
        session: &Session,
        schema: &str,
        object: &SchemaObject,
        depth: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let id = session.id;
        let key = format!("{schema}.{}", object.name);
        let expanded = session.expanded.contains(&key);
        let toggle_key = key.clone();
        let browse_schema = schema.to_string();
        let browse_name = object.name.clone();
        let icon = match object.kind {
            ObjectKind::Table => "table-2",
            ObjectKind::View => "eye",
        };

        let mut rows = vec![
            tree_row(depth)
                .id(SharedString::from(format!("obj-{id}-{key}")))
                .cursor_pointer()
                .hover(|s| s.bg(theme.s3).text_color(theme.text))
                .text_color(theme.t2)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.browse(id, &browse_schema, &browse_name, window, cx)
                }))
                .child(
                    div()
                        .id(SharedString::from(format!("chev-{id}-{key}")))
                        .w(px(12.))
                        .text_color(theme.t3)
                        .hover(|s| s.text_color(theme.text))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_expanded(id, &toggle_key, cx)
                        }))
                        .child(if expanded { "▾" } else { "▸" }),
                )
                .child(
                    svg()
                        .path(assets::icon(icon))
                        .size(px(13.))
                        .flex_none()
                        .text_color(theme.acc),
                )
                .child(div().flex_1().truncate().child(object.name.clone()))
                .child(count(theme, object.columns.len()))
                .into_any_element(),
        ];

        if expanded {
            rows.extend(object.columns.iter().map(|column| {
                tree_row(depth + 1)
                    .text_color(theme.t2)
                    .child(div().flex_1().truncate().child(column.name.clone()))
                    .when(column.primary_key, |el| el.child(env_tag("PK", theme.acc)))
                    .child(
                        div()
                            .font_family(MONO_FONT)
                            .text_size(px(10.))
                            .text_color(theme.t3)
                            .child(column.type_name.to_lowercase()),
                    )
                    .into_any_element()
            }));
        }
        rows
    }
}

fn count(theme: &Theme, n: usize) -> impl IntoElement {
    div()
        .font_family(MONO_FONT)
        .text_size(px(10.5))
        .text_color(theme.t3)
        .child(n.to_string())
}
