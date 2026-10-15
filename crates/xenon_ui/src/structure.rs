//! A table's structure: columns, indexes, keys both ways and its definition.

use std::rc::Rc;

use gpui::{
    AnyElement, App, ClipboardItem, FontWeight, IntoElement, SharedString, Window, div, prelude::*,
    px,
};
use xenon_driver::{ForeignKey, TableInfo};

use crate::theme::{MONO_FONT, Theme};
use crate::widgets::{self, ButtonKind};

/// Schema, table.
pub type OpenTable = Rc<dyn Fn(String, String, &mut Window, &mut App)>;

pub struct View<'a> {
    pub info: &'a TableInfo,
    pub table: &'a str,
    pub is_view: bool,
    /// Spell an identifier for the connection's engine.
    pub ident: &'a dyn Fn(&str) -> String,
    pub open: OpenTable,
}

pub fn render(view: View<'_>, theme: &Theme) -> AnyElement {
    let info = view.info;
    let mut body = div()
        .id("structure")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(22.))
        .px(px(16.))
        .py(px(14.))
        .text_size(px(12.));

    // Columns.
    let link = |key: &ForeignKey| {
        let (schema, table) = (key.ref_schema.clone(), key.ref_table.clone());
        let label = format!("{}.{}", key.ref_table, key.ref_columns.join(", "));
        table_link(theme, label, schema, table, view.open.clone())
    };
    let columns = info.columns.iter().enumerate().map(|(ix, column)| {
        let key = info
            .foreign_keys
            .iter()
            .find(|key| key.columns.len() == 1 && key.columns[0] == column.name);
        row(theme, ix)
            .child(
                div()
                    .w(px(34.))
                    .flex_none()
                    .flex()
                    .gap(px(3.))
                    .when(column.primary_key, |el| el.child(badge(theme, "PK", true)))
                    .when(key.is_some() && !column.primary_key, |el| {
                        el.child(badge(theme, "FK", false))
                    }),
            )
            .child(
                cell(170.)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(column.name.clone()),
            )
            .child(
                cell(150.)
                    .font_family(MONO_FONT)
                    .text_size(px(11.5))
                    .text_color(theme.t2)
                    .child(column.type_name.to_lowercase()),
            )
            .child(
                cell(70.)
                    .text_color(theme.t3)
                    // A primary key is never null, whatever the engine reports.
                    .child(if column.nullable && !column.primary_key {
                        "null"
                    } else {
                        "not null"
                    }),
            )
            .child(
                cell(180.)
                    .font_family(MONO_FONT)
                    .text_size(px(11.5))
                    .text_color(theme.t2)
                    .child(column.default.clone().unwrap_or_default()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .gap(px(10.))
                    .overflow_hidden()
                    .children(key.map(link))
                    .children(
                        column
                            .comment
                            .clone()
                            .map(|comment| div().truncate().text_color(theme.t3).child(comment)),
                    ),
            )
    });
    body = body.child(section(
        theme,
        "Columns",
        info.columns.len(),
        Some(
            header(theme)
                .child(div().w(px(34.)).flex_none())
                .child(cell(170.).child("Name"))
                .child(cell(150.).child("Type"))
                .child(cell(70.).child("Nullable"))
                .child(cell(180.).child("Default"))
                .child(div().flex_1().truncate().child("References")),
        ),
        columns.map(IntoElement::into_any_element).collect(),
    ));

    // Indexes.
    if !info.indexes.is_empty() {
        let rows = info.indexes.iter().enumerate().map(|(ix, index)| {
            row(theme, ix)
                .child(
                    cell(260.)
                        .font_family(MONO_FONT)
                        .text_size(px(11.5))
                        .text_color(theme.text)
                        .child(index.name.clone()),
                )
                .child(
                    cell(300.)
                        .font_family(MONO_FONT)
                        .text_size(px(11.5))
                        .text_color(theme.t2)
                        .child(index.columns.join(", ")),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(4.))
                        .when(index.primary, |el| el.child(badge(theme, "primary", true)))
                        .when(index.unique && !index.primary, |el| {
                            el.child(badge(theme, "unique", false))
                        }),
                )
                .into_any_element()
        });
        body = body.child(section(
            theme,
            "Indexes",
            info.indexes.len(),
            None,
            rows.collect(),
        ));
    }

    // Keys this table holds.
    if !info.foreign_keys.is_empty() {
        let rows = info.foreign_keys.iter().enumerate().map(|(ix, key)| {
            row(theme, ix)
                .child(
                    cell(260.)
                        .font_family(MONO_FONT)
                        .text_size(px(11.5))
                        .text_color(theme.t2)
                        .child(key.columns.join(", ")),
                )
                .child(div().w(px(300.)).flex_none().child(link(key)))
                .child(actions(theme, key))
                .into_any_element()
        });
        body = body.child(section(
            theme,
            "Foreign keys",
            info.foreign_keys.len(),
            None,
            rows.collect(),
        ));
    }

    // Keys elsewhere that point here.
    if !info.referenced_by.is_empty() {
        let rows = info.referenced_by.iter().enumerate().map(|(ix, key)| {
            let label = format!("{}.{}", key.table, key.columns.join(", "));
            row(theme, ix)
                .child(div().w(px(260.)).flex_none().child(table_link(
                    theme,
                    label,
                    key.schema.clone(),
                    key.table.clone(),
                    view.open.clone(),
                )))
                .child(
                    cell(300.)
                        .font_family(MONO_FONT)
                        .text_size(px(11.5))
                        .text_color(theme.t2)
                        .child(format!("→ {}", key.ref_columns.join(", "))),
                )
                .child(actions(theme, key))
                .into_any_element()
        });
        body = body.child(section(
            theme,
            "Referenced by",
            info.referenced_by.len(),
            None,
            rows.collect(),
        ));
    }

    // Definition: the engine's own, or one written from the parts above.
    let (definition, written) = match &info.definition {
        Some(definition) => (definition.trim().to_string(), false),
        None if !view.is_view => (create_table(&view), true),
        None => (String::new(), false),
    };
    if !definition.is_empty() {
        let copy = definition.clone();
        body = body.child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(heading(
                            theme,
                            if view.is_view {
                                "Definition"
                            } else {
                                "Create statement"
                            },
                        ))
                        .when(written, |el| {
                            el.child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(theme.t3)
                                    .child("written by Xenon from the parts above"),
                            )
                        })
                        .child(div().flex_1())
                        .child(
                            widgets::button(
                                theme,
                                "copy-definition",
                                "Copy",
                                ButtonKind::Ghost,
                                true,
                            )
                            .h(px(24.))
                            .px(px(9.))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                            }),
                        ),
                )
                .child(
                    div()
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(theme.s1)
                        .border_1()
                        .border_color(theme.line_2)
                        .font_family(MONO_FONT)
                        .text_size(px(11.5))
                        .text_color(theme.t2)
                        .children(definition.lines().map(|line| {
                            // Keep indentation; GPUI collapses leading spaces otherwise.
                            let shown = line.replace('\t', "    ").replace(' ', "\u{00a0}");
                            div().min_h(px(17.)).child(SharedString::from(shown))
                        })),
                ),
        );
    }
    body.into_any_element()
}

/// A `create table` statement from the columns, keys and indexes, for engines
/// that don't keep one (Postgres, SQL Server).
fn create_table(view: &View<'_>) -> String {
    let ident = view.ident;
    let info = view.info;
    let mut lines: Vec<String> = info
        .columns
        .iter()
        .map(|column| {
            let mut line = format!("  {} {}", ident(&column.name), column.type_name);
            match column.default.as_deref() {
                // How the value is made, not a default expression.
                Some(made) if made.starts_with("generated") || made == "identity" => {
                    line.push(' ');
                    line.push_str(made);
                }
                Some(default) => {
                    line.push_str(" default ");
                    line.push_str(default);
                }
                None => {}
            }
            if !column.nullable && !column.primary_key {
                line.push_str(" not null");
            }
            line
        })
        .collect();
    let names = |columns: &[String]| {
        columns
            .iter()
            .map(|c| ident(c))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if let Some(primary) = info.indexes.iter().find(|i| i.primary) {
        lines.push(format!("  primary key ({})", names(&primary.columns)));
    }
    for key in &info.foreign_keys {
        let mut line = format!(
            "  constraint {} foreign key ({}) references {} ({})",
            ident(&key.name),
            names(&key.columns),
            ident(&key.ref_table),
            names(&key.ref_columns)
        );
        if let Some(action) = &key.on_delete {
            line.push_str(&format!(" on delete {action}"));
        }
        if let Some(action) = &key.on_update {
            line.push_str(&format!(" on update {action}"));
        }
        lines.push(line);
    }
    let mut out = format!(
        "create table {} (\n{}\n);",
        ident(view.table),
        lines.join(",\n")
    );
    for index in info.indexes.iter().filter(|i| !i.primary) {
        out.push_str(&format!(
            "\ncreate {}index {} on {} ({});",
            if index.unique { "unique " } else { "" },
            ident(&index.name),
            ident(view.table),
            names(&index.columns)
        ));
    }
    out
}

fn heading(theme: &Theme, text: &'static str) -> impl IntoElement {
    div()
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.t2)
        .child(text)
}

fn section(
    theme: &Theme,
    title: &'static str,
    count: usize,
    header: Option<gpui::Div>,
    rows: Vec<AnyElement>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .pb(px(8.))
                .child(heading(theme, title))
                .child(
                    div()
                        .font_family(MONO_FONT)
                        .text_size(px(10.5))
                        .text_color(theme.t3)
                        .child(count.to_string()),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .rounded(px(6.))
                .border_1()
                .border_color(theme.line_2)
                .overflow_hidden()
                .children(header)
                .children(rows),
        )
}

fn header(theme: &Theme) -> gpui::Div {
    div()
        .h(px(28.))
        .flex()
        .items_center()
        .gap(px(12.))
        .px(px(12.))
        .bg(theme.s1)
        .border_b_1()
        .border_color(theme.line_2)
        .text_size(px(11.))
        .text_color(theme.t3)
}

fn row(theme: &Theme, ix: usize) -> gpui::Div {
    div()
        .min_h(px(32.))
        .flex()
        .items_center()
        .gap(px(12.))
        .px(px(12.))
        .when(ix > 0, |el| el.border_t_1().border_color(theme.line_2))
}

fn cell(width: f32) -> gpui::Div {
    div().w(px(width)).flex_none().truncate()
}

fn badge(theme: &Theme, text: &'static str, accent: bool) -> impl IntoElement {
    div()
        .px(px(4.))
        .rounded(px(3.))
        .border_1()
        .border_color(if accent { theme.acc } else { theme.line })
        .font_family(MONO_FONT)
        .text_size(px(9.5))
        .text_color(if accent { theme.acc } else { theme.t2 })
        .child(text)
}

fn actions(theme: &Theme, key: &ForeignKey) -> impl IntoElement {
    let words: Vec<String> = [("on delete", &key.on_delete), ("on update", &key.on_update)]
        .into_iter()
        .filter_map(|(when, action)| action.as_ref().map(|a| format!("{when} {a}")))
        .collect();
    div()
        .flex_1()
        .truncate()
        .text_size(px(11.))
        .text_color(theme.t3)
        .child(words.join(" · "))
}

fn table_link(
    theme: &Theme,
    label: String,
    schema: String,
    table: String,
    open: OpenTable,
) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("open-{schema}.{table}-{label}")))
        .flex()
        .items_center()
        .gap(px(4.))
        .flex_none()
        .cursor_pointer()
        .font_family(MONO_FONT)
        .text_size(px(11.5))
        .text_color(theme.acc)
        .hover(|el| el.underline())
        .child(
            gpui::svg()
                .path(crate::assets::icon("arrow-up-right"))
                .size(px(12.))
                .text_color(theme.acc),
        )
        .child(label)
        .on_click(move |_, window, cx| open(schema.clone(), table.clone(), window, cx))
}
