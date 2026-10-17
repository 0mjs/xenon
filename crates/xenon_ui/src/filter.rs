//! The filter builder's conditions, and the WHERE clause they become. The clause
//! goes into the table's WHERE box, so what runs is always visible SQL.

use xenon_driver::{Connection, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    Equals,
    NotEquals,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    Contains,
    StartsWith,
    EndsWith,
    In,
    IsNull,
    IsNotNull,
}

impl Op {
    pub(crate) const ALL: [Op; 12] = [
        Op::Equals,
        Op::NotEquals,
        Op::Less,
        Op::LessOrEqual,
        Op::Greater,
        Op::GreaterOrEqual,
        Op::Contains,
        Op::StartsWith,
        Op::EndsWith,
        Op::In,
        Op::IsNull,
        Op::IsNotNull,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Op::Equals => "=",
            Op::NotEquals => "≠",
            Op::Less => "<",
            Op::LessOrEqual => "≤",
            Op::Greater => ">",
            Op::GreaterOrEqual => "≥",
            Op::Contains => "contains",
            Op::StartsWith => "starts with",
            Op::EndsWith => "ends with",
            Op::In => "is one of",
            Op::IsNull => "is null",
            Op::IsNotNull => "is not null",
        }
    }

    /// Whether the operator takes a value.
    pub(crate) fn takes_value(self) -> bool {
        !matches!(self, Op::IsNull | Op::IsNotNull)
    }
}

/// One condition: a column, how to compare it, and the value as typed.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Condition {
    pub column: String,
    /// Compare as a number (the column is numeric and the value reads as one).
    pub numeric: bool,
    pub op: Op,
    pub value: String,
}

/// The WHERE clause for `conditions`, joined with `and` (or `or` when `any`).
/// Conditions missing a value they need are left out.
pub(crate) fn where_clause(conn: &dyn Connection, conditions: &[Condition], any: bool) -> String {
    let parts: Vec<String> = conditions
        .iter()
        .filter_map(|condition| clause(conn, condition))
        .collect();
    let joiner = if any { " or " } else { " and " };
    if any && parts.len() > 1 {
        // Parenthesised, so it combines safely with anything typed after it.
        format!("({})", parts.join(joiner))
    } else {
        parts.join(joiner)
    }
}

fn clause(conn: &dyn Connection, condition: &Condition) -> Option<String> {
    let column = conn.ident(&condition.column);
    let raw = condition.value.trim();
    if condition.op.takes_value() && raw.is_empty() {
        return None;
    }
    let literal = |text: &str| {
        let value = if condition.numeric && crate::value_editor::is_plain_number(text) {
            Value::Decimal(text.to_string())
        } else {
            Value::Text(text.to_string())
        };
        conn.literal(&value)
    };
    // `like` with the value's own % and _ escaped, so they match themselves;
    // also `[`, a pattern on SQL Server.
    let like = |before: &str, after: &str| {
        let mut escaped = raw
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        if conn.uses_top() {
            escaped = escaped.replace('[', "\\[");
        }
        format!(
            "{column} like {} escape {}",
            conn.literal(&Value::Text(format!("{before}{escaped}{after}"))),
            conn.literal(&Value::Text("\\".into()))
        )
    };
    Some(match condition.op {
        Op::Equals => format!("{column} = {}", literal(raw)),
        Op::NotEquals => format!("{column} <> {}", literal(raw)),
        Op::Less => format!("{column} < {}", literal(raw)),
        Op::LessOrEqual => format!("{column} <= {}", literal(raw)),
        Op::Greater => format!("{column} > {}", literal(raw)),
        Op::GreaterOrEqual => format!("{column} >= {}", literal(raw)),
        Op::Contains => like("%", "%"),
        Op::StartsWith => like("", "%"),
        Op::EndsWith => like("%", ""),
        Op::In => {
            let values: Vec<String> = raw
                .split(',')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(literal)
                .collect();
            if values.is_empty() {
                return None;
            }
            format!("{column} in ({})", values.join(", "))
        }
        Op::IsNull => format!("{column} is null"),
        Op::IsNotNull => format!("{column} is not null"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use xenon_sqlite::SqliteConnection;

    fn condition(column: &str, op: Op, value: &str, numeric: bool) -> Condition {
        Condition {
            column: column.into(),
            numeric,
            op,
            value: value.into(),
        }
    }

    #[test]
    fn conditions_become_a_where_clause() {
        let conn = SqliteConnection::open_in_memory().unwrap();
        let all = [
            condition("status", Op::Equals, "paid", false),
            condition("total", Op::GreaterOrEqual, "100", true),
            condition("note", Op::IsNull, "", false),
            condition("email", Op::Contains, "50%_off", false),
            condition("Order Id", Op::In, "1, 2, x", true),
            // No value yet: left out.
            condition("city", Op::Equals, "  ", false),
        ];
        assert_eq!(
            where_clause(&conn, &all, false),
            "status = 'paid' and total >= 100 and note is null and \
             email like '%50\\%\\_off%' escape '\\' and \"Order Id\" in (1, 2, 'x')"
        );
        assert_eq!(
            where_clause(&conn, &all[..2], true),
            "(status = 'paid' or total >= 100)"
        );
        assert_eq!(
            where_clause(
                &conn,
                &[condition("name", Op::StartsWith, "O'B", false)],
                false
            ),
            "name like 'O''B%' escape '\\'"
        );
    }

    /// SQLite, spelling `like` as for SQL Server, where `[a]` is a pattern.
    struct Brackets(SqliteConnection);

    impl xenon_driver::Connection for Brackets {
        fn name(&self) -> &str {
            self.0.name()
        }
        fn server_version(&self) -> String {
            self.0.server_version()
        }
        fn execute_streaming(
            &self,
            sql: &str,
            row_limit: usize,
            progress: &mut dyn FnMut(xenon_driver::Progress),
        ) -> anyhow::Result<xenon_driver::QueryResult> {
            self.0.execute_streaming(sql, row_limit, progress)
        }
        fn introspect(&self) -> anyhow::Result<Vec<xenon_driver::Schema>> {
            self.0.introspect()
        }
        fn describe(&self, schema: &str, table: &str) -> anyhow::Result<xenon_driver::TableInfo> {
            self.0.describe(schema, table)
        }
        fn cancel_handle(&self) -> std::sync::Arc<dyn xenon_driver::CancelHandle> {
            self.0.cancel_handle()
        }
        fn uses_top(&self) -> bool {
            true
        }
    }

    #[test]
    fn sql_server_brackets_match_themselves() {
        let conn = Brackets(SqliteConnection::open_in_memory().unwrap());
        assert_eq!(
            where_clause(
                &conn,
                &[condition("tag", Op::Contains, "[a]", false)],
                false
            ),
            "tag like '%\\[a]%' escape '\\'"
        );
    }

    #[test]
    fn the_clause_filters_what_it_says() {
        let conn = SqliteConnection::open_in_memory().unwrap();
        conn.execute(
            "create table t (id integer, name text);
             insert into t values (1, 'a_b'), (2, 'axb'), (3, null), (4, '100%')",
            10,
        )
        .unwrap();
        let count = |conditions: &[Condition]| {
            let sql = format!(
                "select count(*) from t where {}",
                where_clause(&conn, conditions, false)
            );
            match conn.execute(&sql, 10).unwrap().outcome {
                xenon_driver::QueryOutcome::Rows(rows) => rows.rows[0][0].clone(),
                other => panic!("{other:?}"),
            }
        };
        // `_` matches only itself, not any character.
        assert_eq!(
            count(&[condition("name", Op::Contains, "_", false)]),
            Value::Integer(1)
        );
        assert_eq!(
            count(&[condition("name", Op::EndsWith, "%", false)]),
            Value::Integer(1)
        );
        assert_eq!(
            count(&[condition("name", Op::IsNull, "", false)]),
            Value::Integer(1)
        );
        assert_eq!(
            count(&[condition("id", Op::In, "1,2", true)]),
            Value::Integer(2)
        );
    }
}

// ---------------------------------------------------------------- the builder

use gpui::{
    AnyElement, Context, Entity, IntoElement, SharedString, Window, deferred, div, prelude::*, px,
};
use gpui_component::input::InputState;

use crate::theme::Theme;
use crate::widgets::{self, ButtonKind};
use crate::workspace::Workspace;

/// The builder's state on a table tab.
pub(crate) struct Builder {
    rows: Vec<Row>,
    any: bool,
    /// The open picker: a row's column or operator list.
    menu: Option<(usize, Menu)>,
}

struct Row {
    column: usize,
    op: Op,
    value: Entity<InputState>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Menu {
    Column,
    Op,
}

/// Type names that hold numbers, so a value like `100` is compared as one:
/// whole names (`interval`, `point` aren't).
fn numeric(type_name: &str) -> bool {
    crate::value_editor::is_numeric_type(type_name)
}

impl Workspace {
    /// The active table tab's columns: name and whether it holds numbers.
    fn filter_columns(&self) -> Vec<(String, bool)> {
        let tab = self.active_tab();
        if let Some(info) = tab.browse.as_ref().and_then(|b| b.info.as_ref()) {
            return info
                .columns
                .iter()
                .map(|c| (c.name.clone(), numeric(&c.type_name)))
                .collect();
        }
        tab.grid_columns()
            .into_iter()
            .map(|(name, type_name)| (name, type_name.as_deref().is_some_and(numeric)))
            .collect()
    }

    fn new_filter_row(&self, window: &mut Window, cx: &mut Context<Self>) -> Row {
        Row {
            column: 0,
            op: Op::Equals,
            value: cx.new(|cx| InputState::new(window, cx).placeholder("value")),
        }
    }

    /// Open the builder with one condition, or close it.
    pub(crate) fn toggle_filter_builder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self
            .active_tab()
            .browse
            .as_ref()
            .is_some_and(|b| b.builder.is_some());
        let builder = (!open).then(|| Builder {
            rows: vec![self.new_filter_row(window, cx)],
            any: false,
            menu: None,
        });
        if let Some(browse) = &mut self.tabs[self.active].browse {
            browse.builder = builder;
        }
        cx.notify();
    }

    fn with_builder(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Builder)) {
        if let Some(builder) = self.tabs[self.active]
            .browse
            .as_mut()
            .and_then(|b| b.builder.as_mut())
        {
            f(builder);
            cx.notify();
        }
    }

    pub(crate) fn add_filter_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let row = self.new_filter_row(window, cx);
        self.with_builder(cx, |b| {
            b.rows.push(row);
            b.menu = None;
        });
    }

    /// Choose a row's column or operator from its list.
    pub(crate) fn pick_filter(
        &mut self,
        row: usize,
        menu: Menu,
        pick: usize,
        cx: &mut Context<Self>,
    ) {
        self.with_builder(cx, |b| {
            if let Some(r) = b.rows.get_mut(row) {
                match menu {
                    Menu::Column => r.column = pick,
                    Menu::Op => r.op = Op::ALL[pick],
                }
            }
            b.menu = None;
        });
    }

    /// The WHERE clause the builder's conditions make.
    fn builder_clause(&self, cx: &Context<Self>) -> Option<String> {
        let builder = self.active_tab().browse.as_ref()?.builder.as_ref()?;
        let conn = self.active_session()?.conn.clone();
        let columns = self.filter_columns();
        let conditions: Vec<Condition> = builder
            .rows
            .iter()
            .filter_map(|row| {
                let (column, numeric) = columns.get(row.column)?.clone();
                Some(Condition {
                    column,
                    numeric,
                    op: row.op,
                    value: row.value.read(cx).value().to_string(),
                })
            })
            .collect();
        Some(where_clause(conn.as_ref(), &conditions, builder.any))
    }

    /// Write the conditions into the WHERE box and run it.
    pub(crate) fn apply_filter_builder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(clause) = self.builder_clause(cx) else {
            return;
        };
        let Some(browse) = &mut self.tabs[self.active].browse else {
            return;
        };
        browse.builder = None;
        let filter = browse.filter.clone();
        filter.update(cx, |input, cx| input.set_value(clause, window, cx));
        self.run_browse(window, cx);
    }

    /// Set a condition's value (for tests, which have no keyboard).
    #[cfg(test)]
    pub(crate) fn set_filter_value(
        &mut self,
        row: usize,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.tabs[self.active]
            .browse
            .as_ref()
            .and_then(|b| b.builder.as_ref())
            .and_then(|b| b.rows.get(row))
            .map(|r| r.value.clone());
        if let Some(input) = input {
            input.update(cx, |input, cx| {
                input.set_value(text.to_string(), window, cx)
            });
        }
    }

    #[cfg(test)]
    pub(crate) fn filter_text(&self, cx: &Context<Self>) -> String {
        self.active_tab()
            .browse
            .as_ref()
            .map(|b| b.filter.read(cx).value().to_string())
            .unwrap_or_default()
    }

    pub(crate) fn render_filter_builder(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let builder = self.active_tab().browse.as_ref()?.builder.as_ref()?;
        let columns = self.filter_columns();
        let small = |button: gpui::Stateful<gpui::Div>| button.h(px(26.)).px(px(10.));
        let picker = |id: SharedString, text: String, width: f32, mono: bool| {
            div()
                .id(id)
                .w(px(width))
                .h(px(28.))
                .px(px(10.))
                .flex()
                .items_center()
                .justify_between()
                .gap(px(6.))
                .rounded(px(6.))
                .bg(theme.field)
                .border_1()
                .border_color(theme.line)
                .cursor_pointer()
                .hover(|el| el.border_color(theme.acc))
                .text_size(crate::theme::text(12.))
                .text_color(theme.text)
                .child(
                    div()
                        .truncate()
                        .when(mono, |el| el.font_family(crate::theme::mono_font()))
                        .child(text),
                )
                .child(
                    div()
                        .text_color(theme.t3)
                        .text_size(crate::theme::text(8.))
                        .child("▼"),
                )
        };
        let list = |row: usize, menu: Menu, items: Vec<String>, mono: bool| {
            deferred(
                div().absolute().top(px(32.)).left_0().child(
                    widgets::overlay_panel(theme)
                        .id(("filter-menu", row))
                        .occlude()
                        .w(px(220.))
                        .max_h(px(280.))
                        .p(px(4.))
                        .overflow_y_scroll()
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.with_builder(cx, |b| b.menu = None);
                        }))
                        .children(items.into_iter().enumerate().map(|(ix, label)| {
                            div()
                                .id(("filter-item", ix))
                                .h(px(26.))
                                .px(px(8.))
                                .flex()
                                .items_center()
                                .rounded(px(4.))
                                .cursor_pointer()
                                .text_size(crate::theme::text(12.))
                                .text_color(theme.text)
                                .when(mono, |el| el.font_family(crate::theme::mono_font()))
                                .hover(|el| el.bg(theme.s2))
                                .child(div().truncate().child(label))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.pick_filter(row, menu, ix, cx)
                                }))
                        })),
                ),
            )
        };
        let toggle = |row: usize, menu: Menu| {
            cx.listener(
                move |this: &mut Self,
                      _: &gpui::ClickEvent,
                      _: &mut Window,
                      cx: &mut Context<Self>| {
                    this.with_builder(cx, |b| {
                        b.menu = (b.menu != Some((row, menu))).then_some((row, menu));
                    })
                },
            )
        };

        let rows: Vec<AnyElement> = builder
            .rows
            .iter()
            .enumerate()
            .map(|(ix, row)| {
                let column = columns
                    .get(row.column)
                    .map_or_else(String::new, |c| c.0.clone());
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .w(px(44.))
                            .text_size(crate::theme::text(11.))
                            .font_family(crate::theme::mono_font())
                            .text_color(theme.t3)
                            .child(if ix == 0 {
                                "WHERE"
                            } else if builder.any {
                                "OR"
                            } else {
                                "AND"
                            }),
                    )
                    .child(
                        div()
                            .relative()
                            .child(
                                picker(format!("filter-column-{ix}").into(), column, 190., true)
                                    .on_click(toggle(ix, Menu::Column)),
                            )
                            .when(builder.menu == Some((ix, Menu::Column)), |el| {
                                el.child(list(
                                    ix,
                                    Menu::Column,
                                    columns.iter().map(|(name, _)| name.clone()).collect(),
                                    true,
                                ))
                            }),
                    )
                    .child(
                        div()
                            .relative()
                            .child(
                                picker(
                                    format!("filter-op-{ix}").into(),
                                    row.op.label().into(),
                                    130.,
                                    false,
                                )
                                .on_click(toggle(ix, Menu::Op)),
                            )
                            .when(builder.menu == Some((ix, Menu::Op)), |el| {
                                el.child(list(
                                    ix,
                                    Menu::Op,
                                    Op::ALL.iter().map(|op| op.label().to_string()).collect(),
                                    false,
                                ))
                            }),
                    )
                    .map(|el| {
                        if row.op.takes_value() {
                            el.child(widgets::text_field(theme, &row.value, false).flex_1())
                        } else {
                            el.child(div().flex_1())
                        }
                    })
                    .child(
                        div()
                            .id(("filter-remove", ix))
                            .size(px(26.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .cursor_pointer()
                            .text_color(theme.t3)
                            .hover(|el| el.bg(theme.s3).text_color(theme.text))
                            .child("×")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.with_builder(cx, |b| {
                                    if b.rows.len() > 1 {
                                        b.rows.remove(ix);
                                    }
                                    b.menu = None;
                                })
                            })),
                    )
                    .into_any_element()
            })
            .collect();

        let any = builder.any;
        let match_picker = widgets::segmented(
            theme,
            "filter-match",
            vec![("Match all", !any), ("Match any", any)],
            cx.listener(|this, ix: &usize, _, cx| {
                let any = *ix == 1;
                this.with_builder(cx, |b| b.any = any);
            }),
        )
        .w(px(200.));
        let preview = self.builder_clause(cx).unwrap_or_default();
        Some(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(6.))
                .px(px(8.))
                .py(px(8.))
                .border_b_1()
                .border_color(theme.line)
                .bg(theme.s1)
                .children(rows)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .pt(px(2.))
                        .child(
                            small(widgets::button(
                                theme,
                                "filter-add",
                                "+ Condition",
                                ButtonKind::Ghost,
                                true,
                            ))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.add_filter_row(window, cx)),
                            ),
                        )
                        .child(match_picker)
                        .child(
                            div()
                                .flex_1()
                                .truncate()
                                .font_family(crate::theme::mono_font())
                                .text_size(crate::theme::text(11.))
                                .text_color(theme.t3)
                                .child(preview),
                        )
                        .child(
                            small(widgets::button(
                                theme,
                                "filter-cancel",
                                "Cancel",
                                ButtonKind::Ghost,
                                true,
                            ))
                            .on_click(cx.listener(
                                |this, _, window, cx| this.toggle_filter_builder(window, cx),
                            )),
                        )
                        .child(
                            small(widgets::button(
                                theme,
                                "filter-apply",
                                "Apply",
                                ButtonKind::Primary,
                                true,
                            ))
                            .on_click(cx.listener(
                                |this, _, window, cx| this.apply_filter_builder(window, cx),
                            )),
                        ),
                )
                .into_any_element(),
        )
    }
}
