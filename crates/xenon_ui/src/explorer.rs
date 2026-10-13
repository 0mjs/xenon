//! The explorer panel: connection, schemas, tables and their columns.

use gpui::{AnyElement, Context, FontWeight, IntoElement, SharedString, div, prelude::*, px};
use xenon_driver::{ObjectKind, SchemaObject};

use crate::theme::{MONO_FONT, Theme};
use crate::widgets::{empty_hint, env_tag, panel_button, tree_row};
use crate::workspace::Workspace;
use crate::{NewConnection, OpenFile};

impl Workspace {
    pub(crate) fn render_explorer(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut list = div().id("explorer-list").flex_1().overflow_y_scroll().pb(px(8.));

        match &self.session {
            None => {
                list = list
                    .child(empty_hint(theme, "No connection"))
                    .child(
                        panel_button(theme, "open-file", "Open SQLite file", "⌘O").on_click(
                            cx.listener(|this, _, window, cx| this.open_file(&OpenFile, window, cx)),
                        ),
                    )
                    .child(
                        panel_button(theme, "connect-pg", "Connect to Postgres", "⌘N").on_click(
                            cx.listener(|this, _, window, cx| {
                                this.new_connection(&NewConnection, window, cx)
                            }),
                        ),
                    )
                    .child(empty_hint(theme, "or drop a SQLite file here"));
            }
            Some(session) => {
                list = list.child(
                    tree_row(0)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(div().w(px(12.)).child("▾"))
                        .child(div().flex_1().truncate().child(session.conn.name().to_string()))
                        .when(session.conn.is_local(), |el| el.child(env_tag("LOCAL", theme.t3))),
                );
                let nested = session.shows_schemas();
                for schema in &session.schemas {
                    let collapsed = nested && self.collapsed_schemas.contains(&schema.name);
                    if nested {
                        let name = schema.name.clone();
                        list = list.child(
                            tree_row(1)
                                .id(SharedString::from(format!("schema-{}", schema.name)))
                                .cursor_pointer()
                                .text_color(theme.t2)
                                .hover(|s| s.bg(theme.s3))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.toggle_schema(&name, cx)
                                }))
                                .child(
                                    div()
                                        .w(px(12.))
                                        .text_color(theme.t3)
                                        .child(if collapsed { "▸" } else { "▾" }),
                                )
                                .child(div().flex_1().truncate().child(schema.name.clone()))
                                .child(count(theme, schema.objects.len())),
                        );
                    }
                    if collapsed {
                        continue;
                    }
                    let depth = if nested { 2 } else { 1 };
                    for object in &schema.objects {
                        list = list.children(self.render_object(&schema.name, object, depth, theme, cx));
                    }
                    if schema.objects.is_empty() {
                        list = list.child(empty_hint(theme, "No tables"));
                    }
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

        list.into_any_element()
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
