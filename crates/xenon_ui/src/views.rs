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
    key: (usize, usize),
    pub editor: Entity<EditorState>,
    pub truncated: bool,
}

fn key(grid: &Grid) -> (usize, usize) {
    (
        Arc::as_ptr(&grid.data) as usize,
        Arc::as_ptr(&grid.order()) as usize,
    )
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
        let key = key(grid);
        if tab.json.as_ref().is_some_and(|j| j.key == key) {
            return;
        }
        let (text, truncated) = json(grid);
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

    /// Grid / Records / JSON, beside Export.
    pub(crate) fn render_view_switch(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let current = self.active_tab().view;
        let this = cx.entity().downgrade();
        widgets::segmented(
            theme,
            "result-view",
            ResultView::ALL
                .into_iter()
                .map(|v| (v.label(), v == current))
                .collect(),
            move |ix, _, cx| {
                let view = ResultView::ALL[*ix];
                this.update(cx, |this, cx| this.set_view(view, cx)).ok();
            },
        )
        .w(px(200.))
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

    pub(crate) fn render_json(&self, theme: &Theme) -> AnyElement {
        let Some(json) = &self.active_tab().json else {
            return div().into_any_element();
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .when(json.truncated, |el| {
                el.child(
                    div()
                        .px(px(14.))
                        .py(px(6.))
                        .text_size(crate::theme::text(11.))
                        .text_color(theme.t3)
                        .border_b_1()
                        .border_color(theme.line_2)
                        .child(format!(
                            "The first {} rows. Export writes them all.",
                            widgets::group_digits(JSON_ROWS)
                        )),
                )
            })
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
