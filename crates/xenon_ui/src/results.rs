//! The results grid: a virtualized list of rows inside a horizontal scroller.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, FontWeight, IntoElement, Pixels, SharedString, Window, div, prelude::*, px,
    uniform_list,
};
use xenon_driver::{ResultSet, Value};

use crate::theme::{MONO_FONT, Theme};

pub const ROW_HEIGHT: f32 = 25.;
const HEADER_HEIGHT: f32 = 32.;
const INDEX_WIDTH: f32 = 48.;
/// Approximate advance of IBM Plex Mono at 11.5px, used to size columns.
const MONO_ADVANCE: f32 = 6.9;
const MIN_COLUMN: f32 = 64.;
const MAX_COLUMN: f32 = 360.;
/// Rows sampled when estimating column widths.
const WIDTH_SAMPLE: usize = 200;
/// Characters shown in a cell; the inspector shows the full value.
const CELL_CHARS: usize = 200;

pub struct Grid {
    pub data: Arc<ResultSet>,
    widths: Arc<Vec<Pixels>>,
    total_width: Pixels,
}

impl Grid {
    pub fn new(data: ResultSet) -> Self {
        let widths: Vec<Pixels> = data
            .columns
            .iter()
            .enumerate()
            .map(|(ix, column)| {
                let header = column
                    .name
                    .chars()
                    .count()
                    .max(column.type_name.as_deref().map_or(0, |t| t.chars().count()));
                let widest = data
                    .rows
                    .iter()
                    .take(WIDTH_SAMPLE)
                    .map(|row| row[ix].display().chars().count().min(CELL_CHARS))
                    .max()
                    .unwrap_or(0)
                    .max(header);
                px((widest as f32 * MONO_ADVANCE + 22.).clamp(MIN_COLUMN, MAX_COLUMN))
            })
            .collect();
        let total_width = widths.iter().fold(px(INDEX_WIDTH), |sum, w| sum + *w);
        Self {
            data: Arc::new(data),
            widths: Arc::new(widths),
            total_width,
        }
    }
}

pub type OnSelect = Rc<dyn Fn(usize, &mut Window, &mut App)>;

pub fn render(
    grid: &Grid,
    selected: Option<usize>,
    theme: &Theme,
    on_select: OnSelect,
) -> impl IntoElement {
    let header = div()
        .h(px(HEADER_HEIGHT))
        .flex_none()
        .flex()
        .flex_row()
        .bg(theme.s1)
        .border_b_1()
        .border_color(theme.line)
        .child(
            div()
                .w(px(INDEX_WIDTH))
                .h_full()
                .border_r_1()
                .border_color(theme.line_2),
        )
        .children(grid.data.columns.iter().zip(grid.widths.iter()).map(
            |(column, width)| {
                div()
                    .w(*width)
                    .h_full()
                    .px(px(10.))
                    .flex()
                    .flex_col()
                    .justify_center()
                    .border_r_1()
                    .border_color(theme.line_2)
                    .overflow_hidden()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(SharedString::from(column.name.clone())),
                    )
                    .child(
                        div()
                            .truncate()
                            .font_family(MONO_FONT)
                            .text_size(px(9.5))
                            .text_color(theme.t3)
                            .child(SharedString::from(
                                column.type_name.clone().unwrap_or_default().to_lowercase(),
                            )),
                    )
            },
        ));

    let data = grid.data.clone();
    let widths = grid.widths.clone();
    let theme = theme.clone();
    let rows = uniform_list("result-rows", data.rows.len(), move |range, _, _| {
        range
            .map(|ix| render_row(ix, &data.rows[ix], &widths, selected, &theme, &on_select))
            .collect()
    })
    .flex_1()
    .w(grid.total_width);

    div().id("result-grid").size_full().overflow_x_scroll().child(
        div()
            .w(grid.total_width)
            .h_full()
            .flex()
            .flex_col()
            .child(header)
            .child(rows),
    )
}

fn render_row(
    ix: usize,
    row: &[Value],
    widths: &[Pixels],
    selected: Option<usize>,
    theme: &Theme,
    on_select: &OnSelect,
) -> AnyElement {
    let is_selected = selected == Some(ix);
    let background = if is_selected {
        Some(theme.acc_wash)
    } else if ix % 2 == 1 {
        Some(theme.s2)
    } else {
        None
    };
    let on_select = on_select.clone();

    div()
        .id(ix)
        .h(px(ROW_HEIGHT))
        .flex()
        .flex_row()
        .font_family(MONO_FONT)
        .text_size(px(11.5))
        .border_b_1()
        .border_color(theme.line_2)
        .when_some(background, |el, bg| el.bg(bg))
        .hover(|style| style.bg(theme.s3))
        .on_click(move |_, window, cx| on_select(ix, window, cx))
        .child(
            div()
                .w(px(INDEX_WIDTH))
                .h_full()
                .pr(px(10.))
                .flex()
                .items_center()
                .justify_end()
                .border_r_1()
                .border_color(theme.line_2)
                .text_color(if is_selected { theme.acc } else { theme.t3 })
                .child(SharedString::from((ix + 1).to_string())),
        )
        .children(row.iter().zip(widths).map(|(value, width)| {
            div()
                .w(*width)
                .h_full()
                .px(px(10.))
                .flex()
                .items_center()
                .when(value.is_numeric(), |el| el.justify_end())
                .border_r_1()
                .border_color(theme.line_2)
                .overflow_hidden()
                .child(cell(value, theme))
        }))
        .into_any_element()
}

fn cell(value: &Value, theme: &Theme) -> impl IntoElement {
    let el = div().truncate();
    if value.is_null() {
        return el
            .text_color(theme.t3)
            .italic()
            .child(SharedString::new_static("NULL"));
    }
    let text: String = value
        .display()
        .chars()
        .take(CELL_CHARS)
        .map(|c| if c == '\n' { '⏎' } else { c })
        .collect();
    el.child(SharedString::from(text))
}
