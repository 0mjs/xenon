//! The explorer panel: connection, schemas, tables and their columns.

use gpui::{AnyElement, Context, FontWeight, IntoElement, SharedString, div, prelude::*, px};
use gpui_component::input::Input;
use xenon_driver::{ObjectKind, SchemaObject};
use xenon_store::{SavedConnection, Target};

use crate::palette::fuzzy_score;
use crate::theme::{MONO_FONT, Theme};
use crate::widgets::{connection_tags, empty_hint, env_tag, panel_button, panel_label, tree_row};
use crate::workspace::Workspace;
use crate::{NewConnection, OpenFile};

impl Workspace {
    pub(crate) fn render_explorer(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut list = div()
            .id("explorer-list")
            .flex_1()
            .overflow_y_scroll()
            .pb(px(8.));

        match &self.session {
            None => {
                list =
                    list.child(empty_hint(theme, "No connection"))
                        .child(
                            panel_button(theme, "open-file", "Open SQLite file", "⌘O").on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.open_file(&OpenFile, window, cx)
                                }),
                            ),
                        )
                        .child(
                            panel_button(theme, "connect-pg", "Connect to Postgres", "⌘N")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.new_connection(&NewConnection, window, cx)
                                })),
                        )
                        .child(empty_hint(theme, "or drop a SQLite file here"));
            }
            Some(session) => {
                let filter = self.explorer_filter.read(cx).value().trim().to_string();
                list = list.child(
                    tree_row(0)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(div().w(px(12.)).child("▾"))
                        .child(div().flex_1().truncate().child(session.name.clone()))
                        .children(
                            connection_tags(
                                theme,
                                session.environment,
                                session.read_only,
                                session.conn.is_local(),
                            )
                            .into_iter()
                            .map(|(label, color)| env_tag(label, color)),
                        ),
                );
                let nested = session.shows_schemas();
                let mut shown = 0;
                for schema in &session.schemas {
                    let objects: Vec<&SchemaObject> = schema
                        .objects
                        .iter()
                        .filter(|o| filter.is_empty() || fuzzy_score(&filter, &o.name).is_some())
                        .collect();
                    if !filter.is_empty() && objects.is_empty() {
                        continue;
                    }
                    // A filter shows every match, even in collapsed schemas.
                    let collapsed = nested
                        && filter.is_empty()
                        && self.collapsed_schemas.contains(&schema.name);
                    if nested {
                        let name = schema.name.clone();
                        list = list.child(
                            tree_row(1)
                                .id(SharedString::from(format!("schema-{}", schema.name)))
                                .cursor_pointer()
                                .text_color(theme.t2)
                                .hover(|s| s.bg(theme.s3))
                                .on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        this.toggle_schema(&name, cx)
                                    }),
                                )
                                .child(div().w(px(12.)).text_color(theme.t3).child(if collapsed {
                                    "▸"
                                } else {
                                    "▾"
                                }))
                                .child(div().flex_1().truncate().child(schema.name.clone()))
                                .child(count(theme, objects.len())),
                        );
                    }
                    if collapsed {
                        continue;
                    }
                    let depth = if nested { 2 } else { 1 };
                    for object in objects {
                        shown += 1;
                        list = list.children(self.render_object(
                            &schema.name,
                            object,
                            depth,
                            theme,
                            cx,
                        ));
                    }
                    if filter.is_empty() && schema.objects.is_empty() {
                        list = list.child(empty_hint(theme, "No tables"));
                    }
                }
                if !filter.is_empty() && shown == 0 {
                    list = list.child(empty_hint(theme, "No matching tables"));
                }
            }
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
            let active = self.session.as_ref().and_then(|s| s.saved_id);
            for saved in &self.saved {
                list = list.child(self.render_saved(saved, active == Some(saved.id), theme, cx));
            }
        }

        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .when(self.session.is_some(), |el| {
                el.child(
                    div()
                        .px(px(10.))
                        .pb(px(6.))
                        .child(Input::new(&self.explorer_filter)),
                )
            })
            .child(list)
            .into_any_element()
    }

    fn render_saved(
        &self,
        saved: &SavedConnection,
        active: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (engine, local) = match &saved.target {
            Target::Sqlite { .. } => ("SQ", true),
            Target::Postgres { host, .. } => (
                "PG",
                matches!(host.as_str(), "" | "localhost" | "127.0.0.1" | "::1"),
            ),
        };
        let group = SharedString::from(format!("saved-{}", saved.id));
        let (connect, edit, forget) = (saved.clone(), saved.clone(), saved.clone());

        tree_row(0)
            .id(group.clone())
            .group(group.clone())
            .cursor_pointer()
            .text_color(if active { theme.text } else { theme.t2 })
            .when(active, |el| el.bg(theme.acc_wash))
            .when(!active, |el| {
                el.hover(|s| s.bg(theme.s3).text_color(theme.text))
            })
            .on_click(cx.listener(move |this, _, _, cx| this.connect_to_saved(&connect, cx)))
            .child(
                div()
                    .w(px(18.))
                    .font_family(MONO_FONT)
                    .text_size(px(9.))
                    .text_color(theme.t3)
                    .child(engine),
            )
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
        schema: &str,
        object: &SchemaObject,
        depth: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let key = format!("{schema}.{}", object.name);
        let expanded = self.expanded.contains(&key);
        let toggle_key = key.clone();
        let browse_schema = schema.to_string();
        let browse_name = object.name.clone();
        let icon = match object.kind {
            ObjectKind::Table => "▦",
            ObjectKind::View => "◇",
        };

        let mut rows = vec![
            tree_row(depth)
                .id(SharedString::from(format!("obj-{key}")))
                .cursor_pointer()
                .hover(|s| s.bg(theme.s3).text_color(theme.text))
                .text_color(theme.t2)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.browse(&browse_schema, &browse_name, window, cx)
                }))
                .child(
                    div()
                        .id(SharedString::from(format!("chev-{key}")))
                        .w(px(12.))
                        .text_color(theme.t3)
                        .hover(|s| s.text_color(theme.text))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_expanded(&toggle_key, cx)
                        }))
                        .child(if expanded { "▾" } else { "▸" }),
                )
                .child(div().text_color(theme.t3).child(icon))
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
