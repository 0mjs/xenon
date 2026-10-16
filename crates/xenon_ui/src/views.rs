//! Other ways to see a result: Records (each row as a card of field and value,
//! which also shows one wide row transposed) and JSON (as Export writes it).

use std::sync::Arc;

use gpui::{
    AnyElement, Context, Entity, ListSizingBehavior, SharedString, Window, div, prelude::*, px,
    uniform_list,
};
use gpui_component::input::{Editor, EditorState};
use xenon_driver::export::{self, Format};

use crate::results::Grid;
use crate::theme::Theme;
use crate::widgets;
use crate::workspace::Workspace;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ResultView {
    #[default]
    Grid,
    Records,
    Json,
}

impl ResultView {
    const ALL: [ResultView; 3] = [ResultView::Grid, ResultView::Records, ResultView::Json];

    fn label(self) -> &'static str {
        match self {
            ResultView::Grid => "Grid",
            ResultView::Records => "Records",
            ResultView::Json => "JSON",
        }
    }
}

/// Rows shown as JSON; Export writes them all.
const JSON_ROWS: usize = 5_000;

/// The JSON view's editor, and which rows (and order) it shows.
pub(crate) struct JsonView {
    key: (usize, usize, bool),
    pub editor: Entity<EditorState>,
    pub truncated: bool,
}

fn key(grid: &Grid) -> (usize, usize) {
    (
        Arc::as_ptr(&grid.data) as usize,
        Arc::as_ptr(&grid.order()) as usize,
    )
}

/// JSON laid out with two-space indents, one value per line, without parsing
/// it: numbers stay exactly as written (a `numeric` with 30 digits, say).
pub(crate) fn pretty(json: &str) -> String {
    let mut out = String::with_capacity(json.len() * 2);
    let mut depth = 0usize;
    let (mut in_string, mut escaped) = (false, false);
    let mut chars = json.chars().peekable();
    let newline = |out: &mut String, depth: usize| {
        out.push('\n');
        out.push_str(&"  ".repeat(depth.min(crate::widgets::MAX_INDENT)));
    };
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '{' | '[' => {
                // Empty stays on one line: `{}`, `[]`.
                let close = if c == '{' { '}' } else { ']' };
                while chars.peek().is_some_and(|n| n.is_whitespace()) {
                    chars.next();
                }
                if chars.peek() == Some(&close) {
                    chars.next();
                    out.push(c);
                    out.push(close);
                } else {
                    out.push(c);
                    depth += 1;
                    newline(&mut out, depth);
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                newline(&mut out, depth);
                out.push(c);
            }
            ',' => {
                out.push(c);
                newline(&mut out, depth);
            }
            ':' => out.push_str(": "),
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
    }
    out.push('\n');
    out
}

/// `grid` as JSON, at most [`JSON_ROWS`] rows, in the order it's shown.
fn json(grid: &Grid) -> (String, bool) {
    let columns: Vec<_> = grid.data.columns.iter().collect();
    let order = grid.order();
    let rows = order
        .iter()
        .take(JSON_ROWS)
        .map(|&ix| grid.data.rows[ix].iter());
    let mut out = Vec::new();
    // Writing to memory can't fail.
    let _ = export::write(Format::Json, &columns, rows, None, &mut out);
    (
        String::from_utf8_lossy(&out).into_owned(),
        order.len() > JSON_ROWS,
    )
}

impl Workspace {
    pub(crate) fn set_view(&mut self, view: ResultView, cx: &mut Context<Self>) {
        self.tabs[self.active].view = view;
        cx.notify();
    }

    /// Build or refresh the active tab's JSON view (it needs a window).
    pub(crate) fn refresh_json_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = &self.tabs[self.active];
        if tab.view != ResultView::Json {
            return;
        }
        let Some(grid) = &tab.grid else {
            return;
        };
        let (data, order) = key(grid);
        let key = (data, order, tab.json_compact);
        if tab.json.as_ref().is_some_and(|j| j.key == key) {
            return;
        }
        let (text, truncated) = json(grid);
        let text = if tab.json_compact {
            text
        } else {
            pretty(&text)
        };
        let editor = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .language("json")
                .line_number(true)
                .searchable(true)
                .default_value(text);
            state.set_readonly(true, cx);
            state
        });
        self.tabs[self.active].json = Some(JsonView {
            key,
            editor,
            truncated,
        });
    }

    /// Grid / Records / JSON, as three icons beside Export.
    pub(crate) fn render_view_switch(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let current = self.active_tab().view;
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(1.))
            .p(px(2.))
            .rounded(px(7.))
            .border_1()
            .border_color(theme.line_2)
            .children(ResultView::ALL.into_iter().enumerate().map(|(ix, view)| {
                let icon = match view {
                    ResultView::Grid => "table-2",
                    ResultView::Records => "list",
                    ResultView::Json => "braces",
                };
                crate::workspace::icon_button(theme, ("result-view", ix), icon, view == current)
                    .size(px(22.))
                    .tooltip(widgets::tooltip(view.label(), ""))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_view(view, cx)))
            }))
            .into_any_element()
    }

    /// Each row as a card of field and value.
    pub(crate) fn render_records(&self, grid: &Grid, theme: &Theme) -> AnyElement {
        let data = grid.data.clone();
        let order = grid.order();
        let theme = theme.clone();
        let names: Arc<Vec<SharedString>> = Arc::new(
            data.columns
                .iter()
                .map(|c| SharedString::from(c.name.clone()))
                .collect(),
        );
        uniform_list("records", order.len(), move |range, _, _| {
            range
                .map(|ix| {
                    let row = &data.rows[order[ix]];
                    div()
                        .w_full()
                        .px(px(14.))
                        .py(px(8.))
                        .border_b_1()
                        .border_color(theme.line_2)
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .h(px(18.))
                                .font_family(crate::theme::mono_font())
                                .text_size(crate::theme::text(10.5))
                                .text_color(theme.t3)
                                .child(format!("ROW {}", ix + 1)),
                        )
                        .children(names.iter().zip(row).map(|(name, value)| {
                            div()
                                .h(px(20.))
                                .flex()
                                .items_center()
                                .gap(px(12.))
                                .child(
                                    div()
                                        .w(px(180.))
                                        .flex_none()
                                        .truncate()
                                        .text_color(theme.t2)
                                        .child(name.clone()),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .truncate()
                                        .font_family(crate::theme::mono_font())
                                        .text_size(crate::theme::text(11.5))
                                        .text_color(if value.is_null() {
                                            theme.t3
                                        } else {
                                            theme.text
                                        })
                                        .child(SharedString::from(value.display().into_owned())),
                                )
                        }))
                })
                .collect()
        })
        .with_sizing_behavior(ListSizingBehavior::Auto)
        .size_full()
        .into_any_element()
    }

    pub(crate) fn render_json(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let tab = self.active_tab();
        let Some(json) = &tab.json else {
            return div().into_any_element();
        };
        let compact = tab.json_compact;
        let this = cx.entity().downgrade();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(10.))
                    .px(px(10.))
                    .py(px(5.))
                    .border_b_1()
                    .border_color(theme.line_2)
                    .child(
                        widgets::segmented(
                            theme,
                            "json-layout",
                            vec![("Pretty", !compact), ("Compact", compact)],
                            move |ix, _, cx| {
                                let compact = *ix == 1;
                                this.update(cx, |this, cx| {
                                    this.tabs[this.active].json_compact = compact;
                                    cx.notify();
                                })
                                .ok();
                            },
                        )
                        .w(px(150.)),
                    )
                    .when(json.truncated, |el| {
                        el.child(
                            div()
                                .text_size(crate::theme::text(11.))
                                .text_color(theme.t3)
                                .child(format!(
                                    "The first {} rows. Export writes them all.",
                                    widgets::group_digits(JSON_ROWS)
                                )),
                        )
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .pt(px(6.))
                    .font_family(crate::theme::mono_font())
                    .text_size(crate::theme::text(12.))
                    .child(Editor::new(&json.editor).h_full().appearance(false)),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_json_keeps_every_character_of_every_value() {
        let compact = r#"[{"id": 1, "total": 12345678901234567890.123456789, "note": "a, {b}: [c]", "tags": [], "meta": {}},
{"id": 2, "total": null, "note": "say \"hi\"", "tags": ["x", "y"], "meta": {"k": true}}]"#;
        let laid_out = pretty(compact);
        assert!(laid_out.contains("\"total\": 12345678901234567890.123456789,"));
        assert!(laid_out.contains("\"note\": \"a, {b}: [c]\","));
        assert!(laid_out.contains("\"tags\": [],"));
        assert!(laid_out.contains("\"note\": \"say \\\"hi\\\"\","));
        assert!(laid_out.starts_with("[\n  {\n    \"id\": 1,"));
        // Still the same JSON.
        let a: serde_json::Value = serde_json::from_str(compact).unwrap();
        let b: serde_json::Value = serde_json::from_str(&laid_out).unwrap();
        assert_eq!(a, b);
    }
}
