//! The results grid: a virtualized list of rows inside a horizontal scroller,
//! with content-sized, resizable columns, cell selection and client-side
//! sorting. Columns keep their own width; stripes and borders run on to the
//! pane's edge, so resizing one column never moves the others' widths.

use std::cmp::Ordering;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, ClickEvent, FontWeight, IntoElement, MouseButton, MouseDownEvent, Pixels,
    SharedString, UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use xenon_driver::sql::SortDirection;
use xenon_driver::{Column, ResultSet, Value};

use crate::theme::{MONO_FONT, Theme};

pub const ROW_HEIGHT: f32 = 25.;
const HEADER_HEIGHT: f32 = 32.;
pub const INDEX_WIDTH: f32 = 48.;
/// Approximate advance of IBM Plex Mono at 11.5px, used to size columns.
const MONO_ADVANCE: f32 = 6.9;
pub const MIN_COLUMN: f32 = 48.;
const MAX_NATURAL: f32 = 360.;
/// Rows sampled when estimating column widths.
const WIDTH_SAMPLE: usize = 200;
/// Characters shown in a cell; the inspector shows the full value.
const CELL_CHARS: usize = 200;
/// The grab area for resizing, straddling a header's right edge.
const RESIZE_HANDLE: f32 = 7.;

pub struct Grid {
    pub data: Arc<ResultSet>,
    /// Content-fit width of each column.
    pub natural: Vec<f32>,
    /// Display order: `order[i]` is the data row shown at position `i`.
    order: Arc<Vec<usize>>,
    pub sort: Option<(usize, SortDirection)>,
    pub scroll: UniformListScrollHandle,
}

impl Grid {
    pub fn new(data: ResultSet) -> Self {
        let natural = data
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
                // Room for the text, padding and the sort arrow.
                (widest as f32 * MONO_ADVANCE + 34.).clamp(64., MAX_NATURAL)
            })
            .collect();
        let order = (0..data.rows.len()).collect();
        Self {
            data: Arc::new(data),
            natural,
            order: Arc::new(order),
            sort: None,
            scroll: UniformListScrollHandle::new(),
        }
    }

    pub fn row_count(&self) -> usize {
        self.order.len()
    }

    /// The data row shown at display position `ix`.
    pub fn row(&self, ix: usize) -> &[Value] {
        &self.data.rows[self.order[ix]]
    }

    /// Sort the rows in place (client side). `None` restores the query's order.
    pub fn set_sort(&mut self, sort: Option<(usize, SortDirection)>) {
        let mut order: Vec<usize> = (0..self.data.rows.len()).collect();
        if let Some((column, direction)) = sort {
            let rows = &self.data.rows;
            order.sort_by(|&a, &b| {
                let (x, y) = (&rows[a][column], &rows[b][column]);
                let ordering = compare(x, y);
                // Nulls stay last in both directions.
                if direction == SortDirection::Ascending || x.is_null() || y.is_null() {
                    ordering
                } else {
                    ordering.reverse()
                }
            });
        }
        self.order = Arc::new(order);
        self.sort = sort;
    }
}

/// Order values the way a reader expects: numbers numerically, text by its
/// characters, nulls after everything.
fn compare(a: &Value, b: &Value) -> Ordering {
    fn number(value: &Value) -> Option<f64> {
        match value {
            Value::Integer(v) => Some(*v as f64),
            Value::Real(v) => Some(*v),
            Value::Decimal(v) => v.trim_start_matches('$').replace(',', "").parse().ok(),
            _ => None,
        }
    }
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Greater,
        (_, Value::Null) => Ordering::Less,
        (Value::Integer(x), Value::Integer(y)) => x.cmp(y),
        _ => match (number(a), number(b)) {
            (Some(x), Some(y)) => x.total_cmp(&y),
            _ => a.display().cmp(&b.display()),
        },
    }
}

/// Final column widths: the user's dragged width where there is one, else the
/// content-fit width.
pub fn layout(natural: &[f32], manual: &[Option<f32>]) -> Vec<f32> {
    natural
        .iter()
        .zip(manual)
        .map(|(n, m)| m.unwrap_or(*n))
        .collect()
}

/// A rectangular block of cells, in display coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (usize, usize),
    pub head: (usize, usize),
}

impl Selection {
    pub fn cell(row: usize, column: usize) -> Self {
        Self {
            anchor: (row, column),
            head: (row, column),
        }
    }

    pub fn rows(&self) -> Range<usize> {
        self.anchor.0.min(self.head.0)..self.anchor.0.max(self.head.0) + 1
    }

    pub fn columns(&self) -> Range<usize> {
        self.anchor.1.min(self.head.1)..self.anchor.1.max(self.head.1) + 1
    }

    fn contains(&self, row: usize, column: usize) -> bool {
        self.rows().contains(&row) && self.columns().contains(&column)
    }

    /// Move (or, when `extend`, grow) by `dr` rows and `dc` columns, clamped to the grid.
    pub fn step(self, dr: isize, dc: isize, extend: bool, rows: usize, columns: usize) -> Self {
        let clamp = |value: usize, delta: isize, len: usize| {
            (value as isize + delta).clamp(0, len.saturating_sub(1) as isize) as usize
        };
        let head = (
            clamp(self.head.0, dr, rows),
            clamp(self.head.1, dc, columns),
        );
        if extend {
            Self { head, ..self }
        } else {
            Self { anchor: head, head }
        }
    }
}

/// Row, column, extend (shift held).
pub type SelectCallback = Rc<dyn Fn(usize, usize, bool, &mut Window, &mut App)>;
/// A column index.
pub type ColumnCallback = Rc<dyn Fn(usize, &mut Window, &mut App)>;
/// Column, mouse x, current width.
pub type ResizeCallback = Rc<dyn Fn(usize, Pixels, f32, &mut Window, &mut App)>;

pub struct Callbacks {
    pub select: SelectCallback,
    pub sort: ColumnCallback,
    pub resize_start: ResizeCallback,
    pub autofit: ColumnCallback,
}

pub struct View<'a> {
    pub grid: &'a Grid,
    pub widths: Vec<f32>,
    pub selection: Option<Selection>,
    /// Sort shown in the headers (client-side or the browse query's).
    pub sort: Option<(usize, SortDirection)>,
}

pub fn render(view: View<'_>, theme: &Theme, callbacks: Callbacks) -> impl IntoElement {
    let total_width = px(INDEX_WIDTH + view.widths.iter().sum::<f32>());
    let callbacks = Rc::new(callbacks);
    let columns = &view.grid.data.columns;

    let header = div()
        .h(px(HEADER_HEIGHT))
        .w_full()
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
                .flex_none()
                .border_r_1()
                .border_color(theme.line_2),
        )
        .children(
            columns
                .iter()
                .zip(&view.widths)
                .enumerate()
                .map(|(ix, (column, width))| {
                    let direction = view.sort.filter(|(c, _)| *c == ix).map(|(_, d)| d);
                    header_cell(ix, column, *width, direction, theme, &callbacks)
                }),
        )
        .child(div().flex_1());

    let data = view.grid.data.clone();
    let order = view.grid.order.clone();
    let widths = Arc::new(view.widths);
    let selection = view.selection;
    let theme = theme.clone();
    let row_callbacks = callbacks.clone();
    let rows = uniform_list("result-rows", order.len(), move |range, _, _| {
        range
            .map(|ix| {
                render_row(
                    ix,
                    &data.rows[order[ix]],
                    &widths,
                    selection,
                    &theme,
                    &row_callbacks,
                )
            })
            .collect()
    })
    .track_scroll(&view.grid.scroll)
    .flex_1()
    .w_full()
    .min_w(total_width);

    div()
        .id("result-grid")
        .size_full()
        .overflow_x_scroll()
        .child(
            div()
                .w_full()
                .min_w(total_width)
                .h_full()
                .flex()
                .flex_col()
                .child(header)
                .child(rows),
        )
}

fn header_cell(
    ix: usize,
    column: &Column,
    width: f32,
    direction: Option<SortDirection>,
    theme: &Theme,
    callbacks: &Rc<Callbacks>,
) -> impl IntoElement {
    let sort = callbacks.sort.clone();
    let resize = callbacks.resize_start.clone();
    let autofit = callbacks.autofit.clone();
    let arrow = match direction {
        Some(SortDirection::Ascending) => "▲",
        Some(SortDirection::Descending) => "▼",
        None => "",
    };

    div()
        .id(("header", ix))
        .w(px(width))
        .h_full()
        .flex_none()
        .relative()
        .pl(px(10.))
        .pr(px(6.))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.))
        .border_r_1()
        .border_color(theme.line_2)
        .cursor_pointer()
        .hover(|s| s.bg(theme.s3))
        .on_click(move |_, window, cx| sort(ix, window, cx))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
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
                ),
        )
        .child(div().text_size(px(8.)).text_color(theme.acc).child(arrow))
        .child(
            div()
                .id(("resize", ix))
                .absolute()
                .top_0()
                .bottom_0()
                .right(px(-RESIZE_HANDLE / 2.))
                .w(px(RESIZE_HANDLE))
                .cursor_col_resize()
                .on_mouse_down(
                    MouseButton::Left,
                    move |event: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        if event.click_count >= 2 {
                            autofit(ix, window, cx);
                        } else {
                            resize(ix, event.position.x, width, window, cx);
                        }
                    },
                )
                .on_click(|_, _, cx| cx.stop_propagation()),
        )
}

fn render_row(
    ix: usize,
    row: &[Value],
    widths: &[f32],
    selection: Option<Selection>,
    theme: &Theme,
    callbacks: &Rc<Callbacks>,
) -> AnyElement {
    let row_selected = selection.is_some_and(|s| s.rows().contains(&ix));
    let background = if ix % 2 == 1 { Some(theme.s2) } else { None };

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
        .child(
            div()
                .w(px(INDEX_WIDTH))
                .h_full()
                .flex_none()
                .pr(px(10.))
                .flex()
                .items_center()
                .justify_end()
                .border_r_1()
                .border_color(theme.line_2)
                .text_color(if row_selected { theme.acc } else { theme.t3 })
                .child(SharedString::from((ix + 1).to_string())),
        )
        .children(
            row.iter()
                .zip(widths)
                .enumerate()
                .map(|(column, (value, width))| {
                    let selected = selection.is_some_and(|s| s.contains(ix, column));
                    let focused = selection.is_some_and(|s| s.head == (ix, column));
                    let select = callbacks.select.clone();
                    div()
                        .id(("cell", column))
                        .w(px(*width))
                        .h_full()
                        .flex_none()
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .when(value.is_numeric(), |el| el.justify_end())
                        .border_r_1()
                        .border_color(theme.line_2)
                        .overflow_hidden()
                        .when(selected, |el| el.bg(theme.acc_wash))
                        .when(focused, |el| el.border_1().border_color(theme.acc))
                        .on_click(move |event: &ClickEvent, window, cx| {
                            select(ix, column, event.modifiers().shift, window, cx)
                        })
                        .child(cell(value, theme))
                }),
        )
        .child(div().flex_1())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_uses_manual_widths_over_natural() {
        assert_eq!(layout(&[100., 200.], &[None, Some(50.)]), [100., 50.]);
    }

    #[test]
    fn sort_orders_numbers_text_and_nulls() {
        let column = |name: &str| Column {
            name: name.into(),
            type_name: None,
        };
        let data = ResultSet {
            columns: vec![column("n"), column("t")],
            rows: vec![
                vec![Value::Integer(10), Value::Text("b".into())],
                vec![Value::Null, Value::Text("a".into())],
                vec![Value::Integer(2), Value::Null],
                vec![Value::Integer(33), Value::Text("c".into())],
            ],
            truncated: false,
        };
        let mut grid = Grid::new(data);
        let firsts = |grid: &Grid| -> Vec<Value> {
            (0..grid.row_count())
                .map(|ix| grid.row(ix)[0].clone())
                .collect()
        };

        grid.set_sort(Some((0, SortDirection::Ascending)));
        assert_eq!(
            firsts(&grid),
            [
                Value::Integer(2),
                Value::Integer(10),
                Value::Integer(33),
                Value::Null
            ]
        );
        grid.set_sort(Some((0, SortDirection::Descending)));
        assert_eq!(
            firsts(&grid),
            [
                Value::Integer(33),
                Value::Integer(10),
                Value::Integer(2),
                Value::Null
            ]
        );
        grid.set_sort(Some((1, SortDirection::Descending)));
        assert_eq!(grid.row(0)[1], Value::Text("c".into()));
        assert_eq!(grid.row(3)[1], Value::Null);
        grid.set_sort(None);
        assert_eq!(grid.row(0)[0], Value::Integer(10));
    }

    #[test]
    fn decimals_sort_numerically() {
        assert_eq!(
            compare(
                &Value::Decimal("9.5".into()),
                &Value::Decimal("10.25".into())
            ),
            Ordering::Less
        );
    }

    #[test]
    fn selection_steps_and_extends_within_bounds() {
        let s = Selection::cell(0, 0);
        assert_eq!(s.step(-1, -1, false, 10, 3), Selection::cell(0, 0));
        let s = s.step(2, 1, true, 10, 3);
        assert_eq!((s.rows(), s.columns()), (0..3, 0..2));
        let s = s.step(0, 5, false, 10, 3);
        assert_eq!(s, Selection::cell(2, 2));
    }
}
