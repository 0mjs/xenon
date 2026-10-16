//! Query parameters: `:name` (every engine), `$1` (Postgres, DuckDB) and `?`
//! (MySQL, SQLite, DuckDB). Running a query that has them asks for their
//! values first, filled in with what was used last time, then writes each in
//! as a SQL literal and runs the result like any other query: so the
//! production prompt sees, and asks about, exactly what will run.

use gpui::{AnyElement, Context, Entity, FontWeight, Subscription, Window, div, prelude::*, px};
use gpui_component::input::{InputEvent, InputState};
use xenon_driver::Value;
use xenon_driver::sql::{Param, ParamStyle, bind, parameters};

use crate::theme::Theme;
use crate::widgets::{self, ButtonKind, Engine};
use crate::workspace::{RunScope, Workspace};

/// Waiting for the values of a query's parameters.
pub(crate) struct Prompt {
    tab_id: usize,
    sql: String,
    scope: RunScope,
    params: Vec<Param>,
    /// One field per distinct parameter, in the order they first appear.
    fields: Vec<(String, Entity<InputState>)>,
    _subscriptions: Vec<Subscription>,
}

/// Which placeholders an engine takes.
pub(crate) fn style(engine: Engine) -> ParamStyle {
    match engine {
        Engine::Postgres => ParamStyle {
            dollar: true,
            ..Default::default()
        },
        Engine::MySql | Engine::MariaDb => ParamStyle {
            question: true,
            mysql: true,
            ..Default::default()
        },
        Engine::Sqlite => ParamStyle {
            question: true,
            brackets: true,
            ..Default::default()
        },
        Engine::DuckDb => ParamStyle {
            dollar: true,
            question: true,
            ..Default::default()
        },
        // `@name` on SQL Server is a variable, not a placeholder.
        Engine::SqlServer => ParamStyle {
            brackets: true,
            ..Default::default()
        },
        Engine::Unknown => ParamStyle::default(),
    }
}

/// A typed value as it goes into the query: numbers and NULL as they are, text
/// quoted. Quotes around it (`'42'`) keep a number as text.
pub(crate) fn value_of(text: &str) -> Value {
    let trimmed = text.trim();
    if trimmed.eq_ignore_ascii_case("null") {
        return Value::Null;
    }
    if let Some(inner) = trimmed
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
    {
        return Value::Text(inner.replace("''", "'"));
    }
    if let Ok(n) = trimmed.parse::<i64>() {
        return Value::Integer(n);
    }
    let digits = trimmed.strip_prefix('-').unwrap_or(trimmed);
    let decimal = !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit() || c == '.')
        && digits.matches('.').count() == 1
        && !digits.starts_with('.')
        && !digits.ends_with('.');
    if decimal {
        return Value::Decimal(trimmed.to_string());
    }
    Value::Text(text.to_string())
}

fn remembered_key(key: &str) -> String {
    format!("param{key}")
}

impl Workspace {
    /// If `sql` has parameters, ask for them and return true; the query runs
    /// once they're filled in.
    pub(crate) fn ask_parameters(
        &mut self,
        tab_id: usize,
        sql: &str,
        scope: RunScope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(session) = self
            .tabs
            .iter()
            .find(|t| t.id == tab_id)
            .and_then(|t| t.session)
            .and_then(|id| self.session(id))
        else {
            return false;
        };
        let params = parameters(sql, style(session.engine));
        if params.is_empty() {
            return false;
        }
        let remember = self.keeps_history(session);
        let mut fields: Vec<(String, Entity<InputState>)> = Vec::new();
        let mut subscriptions = Vec::new();
        for param in &params {
            if fields.iter().any(|(key, _)| *key == param.key) {
                continue;
            }
            let last = remember
                .then(|| {
                    self.store
                        .as_ref()
                        .and_then(|s| s.setting(&remembered_key(&param.key)).ok().flatten())
                })
                .flatten()
                .unwrap_or_default();
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("value")
                    .default_value(last)
            });
            subscriptions.push(cx.subscribe_in(
                &input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.finish_parameters(window, cx);
                    }
                },
            ));
            fields.push((param.key.clone(), input));
        }
        if let Some((_, first)) = fields.first() {
            first.update(cx, |input, cx| input.focus(window, cx));
        }
        self.parameters = Some(Prompt {
            tab_id,
            sql: sql.to_string(),
            scope,
            params,
            fields,
            _subscriptions: subscriptions,
        });
        cx.notify();
        true
    }

    pub(crate) fn finish_parameters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.parameters.take() else {
            return;
        };
        let Some(session) = self
            .tabs
            .iter()
            .find(|t| t.id == prompt.tab_id)
            .and_then(|t| t.session)
            .and_then(|id| self.session(id))
        else {
            return;
        };
        let conn = session.conn.clone();
        let remember = self.keeps_history(session);
        let values: Vec<(String, String)> = prompt
            .fields
            .iter()
            .map(|(key, input)| (key.clone(), input.read(cx).value().to_string()))
            .collect();
        if let (true, Some(store)) = (remember, &self.store) {
            for (key, value) in &values {
                let _ = store.set_setting(&remembered_key(key), value);
            }
        }
        let sql = bind(&prompt.sql, &prompt.params, |key| {
            let text = values
                .iter()
                .find(|(k, _)| k == key)
                .map_or("", |(_, v)| v.as_str());
            conn.literal(&value_of(text))
        });
        // Positions in the filled-in SQL don't match the editor's, so errors
        // aren't pointed at in it.
        self.run_sql(prompt.tab_id, sql, None, prompt.scope, window, cx);
    }

    pub(crate) fn cancel_parameters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.parameters = None;
        self.focus_editor(window, cx);
        cx.notify();
    }

    pub(crate) fn render_parameters(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let prompt = self.parameters.as_ref()?;
        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_start()
                .justify_center()
                .pt(px(120.))
                .bg(theme.scrim)
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.cancel_parameters(window, cx)),
                )
                .child(
                    widgets::overlay_panel(theme)
                        .key_context("Parameters")
                        .on_action(cx.listener(
                            |this, _: &gpui_component::input::Escape, window, cx| {
                                this.cancel_parameters(window, cx)
                            },
                        ))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .w(px(460.))
                        .h_auto()
                        .p(px(16.))
                        .gap(px(12.))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(
                                    div()
                                        .text_size(crate::theme::text(14.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.text)
                                        .child("Parameters"),
                                )
                                .child(
                                    div()
                                        .text_size(crate::theme::text(11.5))
                                        .text_color(theme.t3)
                                        .child(
                                            "Numbers and NULL go in as they are; anything else is \
                                     quoted. Quote a number ('42') to send it as text.",
                                        ),
                                ),
                        )
                        .children(prompt.fields.iter().map(|(key, input)| {
                            div()
                                .flex()
                                .items_center()
                                .gap(px(10.))
                                .child(
                                    div()
                                        .w(px(110.))
                                        .flex_none()
                                        .truncate()
                                        .font_family(crate::theme::mono_font())
                                        .text_size(crate::theme::text(11.5))
                                        .text_color(theme.t2)
                                        .child(key.clone()),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .child(widgets::text_field(theme, input, false)),
                                )
                        }))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .child(
                                    widgets::button(
                                        theme,
                                        "params-cancel",
                                        "Cancel",
                                        ButtonKind::Ghost,
                                        true,
                                    )
                                    .on_click(cx.listener(
                                        |this, _, window, cx| this.cancel_parameters(window, cx),
                                    )),
                                )
                                .child(
                                    widgets::button(
                                        theme,
                                        "params-run",
                                        "Run",
                                        ButtonKind::Primary,
                                        true,
                                    )
                                    .on_click(cx.listener(
                                        |this, _, window, cx| this.finish_parameters(window, cx),
                                    )),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    /// Fill in a prompt's fields, for tests.
    #[cfg(test)]
    pub(crate) fn fill_parameters(
        &mut self,
        values: &[&str],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(prompt) = &self.parameters {
            for ((_, input), value) in prompt.fields.iter().zip(values) {
                input.update(cx, |input, cx| {
                    input.set_value(value.to_string(), window, cx)
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_values_become_the_right_literals() {
        assert_eq!(value_of("42"), Value::Integer(42));
        assert_eq!(value_of(" -7 "), Value::Integer(-7));
        assert_eq!(value_of("19.99"), Value::Decimal("19.99".into()));
        assert_eq!(value_of("NULL"), Value::Null);
        assert_eq!(value_of("null"), Value::Null);
        assert_eq!(value_of("'42'"), Value::Text("42".into()));
        assert_eq!(value_of("'it''s'"), Value::Text("it's".into()));
        assert_eq!(value_of("paid"), Value::Text("paid".into()));
        assert_eq!(value_of("1.2.3"), Value::Text("1.2.3".into()));
        assert_eq!(value_of(""), Value::Text(String::new()));
    }
}
