//! The results grid: a virtualized list of rows inside a horizontal scroller,
//! with content-sized, resizable columns, cell selection and client-side
//! sorting. Columns keep their own width; stripes and borders run on to the
//! pane's edge, so resizing one column never moves the others' widths.

use std::cell::Cell;
use std::cmp::Ordering;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, FontWeight, IntoElement, MouseButton, MouseDownEvent, Pixels, Rgba,
    ScrollHandle, SharedString, UniformListScrollHandle, Window, canvas, div, prelude::*, px, svg,
    uniform_list,
};
use xenon_driver::sql::SortDirection;
use xenon_driver::{Column, ResultSet, Value};

use crate::assets;
use crate::edits::{Edits, RowRef};
use crate::theme::Theme;

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
    /// The columns shown, in display order (data indexes): hidden ones are left
    /// out, and the first `pinned` stay in view as the grid scrolls sideways.
    columns: Arc<Vec<usize>>,
    pub pinned: usize,
    pub scroll: UniformListScrollHandle,
    /// The sideways scroll, so pinned columns can stay put.
    pub h_scroll: ScrollHandle,
    /// Window x of the header's left edge (after horizontal scrolling), so a
    /// click on a row can be mapped to a column without a handler per cell.
    origin_x: Rc<Cell<Pixels>>,
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
        let columns = (0..data.columns.len()).collect();
        Self {
            data: Arc::new(data),
            natural,
            order: Arc::new(order),
            sort: None,
            columns: Arc::new(columns),
            pinned: 0,
            scroll: UniformListScrollHandle::new(),
            h_scroll: ScrollHandle::new(),
            origin_x: Rc::new(Cell::new(px(0.))),
        }
    }

    pub fn row_count(&self) -> usize {
        self.order.len()
    }

    /// Data row indexes in display order.
    pub fn order(&self) -> Arc<Vec<usize>> {
        self.order.clone()
    }

    /// The data row shown at display position `ix`.
    pub fn row(&self, ix: usize) -> &[Value] {
        &self.data.rows[self.order[ix]]
    }

    /// Which data row is shown at display position `ix`.
    pub fn data_index(&self, ix: usize) -> usize {
        self.order[ix]
    }

    /// The columns shown, in display order (data indexes).
    pub fn columns(&self) -> &[usize] {
        &self.columns
    }

    /// The data column shown at display position `ix`.
    pub fn column(&self, ix: usize) -> Option<usize> {
        self.columns.get(ix).copied()
    }

    /// Where data column `column` is shown, if it is.
    pub fn display_of(&self, column: usize) -> Option<usize> {
        self.columns.iter().position(|&c| c == column)
    }

    /// The value at display row `row`, display column `column`.
    pub fn cell(&self, row: usize, column: usize) -> &Value {
        &self.row(row)[self.columns[column]]
    }

    /// How many columns are hidden.
    pub fn hidden(&self) -> usize {
        self.data.columns.len() - self.columns.len()
    }

    /// Hide the column at display position `ix` (never the last one shown).
    pub fn hide(&mut self, ix: usize) {
        if self.columns.len() > 1 && ix < self.columns.len() {
            Arc::make_mut(&mut self.columns).remove(ix);
            if ix < self.pinned {
                self.pinned -= 1;
            }
        }
    }

    /// Show every column, in the query's order, none pinned.
    pub fn reset_columns(&mut self) {
        self.columns = Arc::new((0..self.data.columns.len()).collect());
        self.pinned = 0;
    }

    /// Move the column at display position `ix` one place left or right,
    /// within the pinned or unpinned ones.
    pub fn move_column(&mut self, ix: usize, right: bool) -> Option<usize> {
        let to = if right { ix + 1 } else { ix.checked_sub(1)? };
        let same_side = (ix < self.pinned) == (to < self.pinned);
        if to >= self.columns.len() || !same_side {
            return None;
        }
        Arc::make_mut(&mut self.columns).swap(ix, to);
        Some(to)
    }

    /// Pin the column at `ix` (it joins the pinned ones on the left) or unpin it
    /// (it goes back to just after them). Returns where it's now shown.
    pub fn toggle_pin(&mut self, ix: usize) -> usize {
        let columns = Arc::make_mut(&mut self.columns);
        let column = columns.remove(ix);
        if ix < self.pinned {
            self.pinned -= 1;
            columns.insert(self.pinned, column);
            self.pinned
        } else {
            columns.insert(self.pinned, column);
            self.pinned += 1;
            self.pinned - 1
        }
    }

    /// The column layout by name, to remember for a table.
    pub fn layout(&self) -> ColumnLayout {
        let names = |ixs: &mut dyn Iterator<Item = usize>| {
            ixs.map(|c| self.data.columns[c].name.clone()).collect()
        };
        ColumnLayout {
            shown: names(&mut self.columns.iter().copied()),
            pinned: self.pinned,
            hidden: names(&mut (0..self.data.columns.len()).filter(|c| !self.columns.contains(c))),
        }
    }

    /// Apply a remembered layout: columns it names, in its order; columns it
    /// doesn't know (new since) shown after them.
    pub fn apply_layout(&mut self, layout: &ColumnLayout) {
        let find = |name: &String| self.data.columns.iter().position(|c| &c.name == name);
        let mut columns: Vec<usize> = layout.shown.iter().filter_map(find).collect();
        let pinned = layout.pinned.min(columns.len());
        let known: Vec<usize> = layout.hidden_and_shown(&self.data.columns);
        columns.extend((0..self.data.columns.len()).filter(|c| !known.contains(c)));
        if columns.is_empty() {
            return;
        }
        self.columns = Arc::new(columns);
        self.pinned = pinned;
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

/// Which columns a table shows, in what order, and how many are pinned:
/// remembered by name, so it survives columns being added or dropped.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ColumnLayout {
    pub shown: Vec<String>,
    pub pinned: usize,
    /// Columns hidden when it was saved (so a new column isn't taken for one).
    #[serde(default)]
    pub hidden: Vec<String>,
}

impl ColumnLayout {
    /// Data indexes of the columns this layout knows about, shown or hidden.
    fn hidden_and_shown(&self, columns: &[Column]) -> Vec<usize> {
        columns
            .iter()
            .enumerate()
            .filter(|(_, c)| self.shown.contains(&c.name) || self.hidden.contains(&c.name))
            .map(|(ix, _)| ix)
            .collect()
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
pub type ColumnPairCallback = Rc<dyn Fn(usize, usize, &mut Window, &mut App)>;
/// Display column, where the click was.
pub type MenuCallback = Rc<dyn Fn(usize, gpui::Point<Pixels>, &mut Window, &mut App)>;
pub type ResizeCallback = Rc<dyn Fn(usize, Pixels, f32, &mut Window, &mut App)>;

pub struct Callbacks {
    pub select: SelectCallback,
    /// A double-click on a cell (display row, column).
    pub edit: ColumnPairCallback,
    pub sort: ColumnCallback,
    /// A click on a foreign-key cell's arrow (display row, column).
    pub follow: ColumnPairCallback,
    pub resize_start: ResizeCallback,
    pub autofit: ColumnCallback,
    /// A right-click on a header (display column, where).
    pub column_menu: MenuCallback,
}

pub struct View<'a> {
    pub grid: &'a Grid,
    pub widths: Vec<f32>,
    pub selection: Option<Selection>,
    /// Sort shown in the headers (client-side or the browse query's).
    pub sort: Option<(usize, SortDirection)>,
    /// Staged changes to show: edited cells, deleted rows, rows being added
    /// (drawn after the fetched ones).
    pub edits: Rc<Edits>,
    /// Per column, the table its values point at, for foreign-key columns.
    pub links: Vec<Option<SharedString>>,
}

pub fn render(view: View<'_>, theme: &Theme, callbacks: Callbacks) -> impl IntoElement {
    let shown = view.grid.columns.clone();
    // Widths in display order.
    let widths: Vec<f32> = shown.iter().map(|&c| view.widths[c]).collect();
    let total_width = px(INDEX_WIDTH + widths.iter().sum::<f32>());
    let callbacks = Rc::new(callbacks);
    let columns = &view.grid.data.columns;
    let pinned = view.grid.pinned.min(shown.len());
    let pinned_width: f32 = widths[..pinned].iter().sum();
    // How far the grid is scrolled sideways: pinned columns move back by it.
    let shift = -f32::from(view.grid.h_scroll.offset().x);

    let header = div()
        .relative()
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
            shown
                .iter()
                .zip(&widths)
                .enumerate()
                .map(|(ix, (&data, width))| {
                    let direction = view.sort.filter(|(c, _)| *c == data).map(|(_, d)| d);
                    let link = view.links.get(data).cloned().flatten();
                    header_cell(
                        ix,
                        data,
                        &columns[data],
                        *width,
                        direction,
                        link,
                        theme,
                        &callbacks,
                    )
                }),
        )
        .child(div().flex_1())
        // Scrolled sideways: the row-number gutter and pinned headers stay put.
        .when(shift > 0., |el| {
            el.child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(shift))
                    .flex()
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
                    .children(shown[..pinned].iter().zip(&widths).enumerate().map(
                        |(ix, (&data, width))| {
                            let direction = view.sort.filter(|(c, _)| *c == data).map(|(_, d)| d);
                            let link = view.links.get(data).cloned().flatten();
                            header_cell(
                                ix,
                                data,
                                &columns[data],
                                *width,
                                direction,
                                link,
                                theme,
                                &callbacks,
                            )
                        },
                    ))
                    .when(pinned > 0, |el| el.border_r_2().border_color(theme.line)),
            )
        })
        .child({
            let origin_x = view.grid.origin_x.clone();
            canvas(
                move |bounds, _, _| origin_x.set(bounds.origin.x),
                |_, _, _, _| {},
            )
            .absolute()
            .size_full()
        });

    let data = view.grid.data.clone();
    let origin_x = view.grid.origin_x.clone();
    let order = view.grid.order.clone();
    let widths = Arc::new(widths);
    let selection = view.selection;
    let layout = Arc::new(RowLayout {
        shown: shown.clone(),
        pinned,
        pinned_width,
        shift,
    });
    let theme = theme.clone();
    let row_callbacks = callbacks.clone();
    let edits = view.edits;
    let links: Arc<Vec<bool>> = Arc::new(view.links.iter().map(Option::is_some).collect());
    let count = order.len() + edits.inserted.len();
    let rows = uniform_list("result-rows", count, move |range, _, _| {
        range
            .map(|ix| {
                let (row, cells) = match order.get(ix) {
                    Some(&data_ix) => (
                        RowRef::Existing(data_ix),
                        data.rows[data_ix]
                            .iter()
                            .enumerate()
                            .map(
                                |(col, value)| match edits.value(RowRef::Existing(data_ix), col) {
                                    Some(Some(new)) => Shown::Edited(new.clone()),
                                    _ => Shown::Value(value.clone()),
                                },
                            )
                            .collect::<Vec<_>>(),
                    ),
                    None => {
                        let new = ix - order.len();
                        (
                            RowRef::New(new),
                            edits.inserted[new]
                                .iter()
                                .map(|value| match value {
                                    Some(value) => Shown::Edited(value.clone()),
                                    None => Shown::Default,
                                })
                                .collect(),
                        )
                    }
                };
                let deleted = matches!(row, RowRef::Existing(r) if edits.deleted.contains(&r));
                render_row(
                    ix,
                    &cells,
                    RowState {
                        new: matches!(row, RowRef::New(_)),
                        deleted,
                    },
                    &layout,
                    &widths,
                    &links,
                    selection,
                    &theme,
                    &row_callbacks,
                    &origin_x,
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
        .track_scroll(&view.grid.h_scroll)
        // Without this a vertical swipe also scrolls sideways (GPUI turns the
        // vertical delta into horizontal scroll for x-only containers), and a
        // trackpad's slight sideways drift moves the grid. With it, each
        // gesture follows its main direction.
        .restrict_scroll_to_axis()
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

#[allow(clippy::too_many_arguments)]
fn header_cell(
    ix: usize,
    data: usize,
    column: &Column,
    width: f32,
    direction: Option<SortDirection>,
    link: Option<SharedString>,
    theme: &Theme,
    callbacks: &Rc<Callbacks>,
) -> impl IntoElement {
    let sort = callbacks.sort.clone();
    let resize = callbacks.resize_start.clone();
    let autofit = callbacks.autofit.clone();
    let menu = callbacks.column_menu.clone();
    let arrow = match direction {
        Some(SortDirection::Ascending) => "▲",
        Some(SortDirection::Descending) => "▼",
        None => "",
    };

    div()
        .id(("header", data))
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
        .on_click(move |_, window, cx| sort(data, window, cx))
        .on_mouse_down(
            MouseButton::Right,
            move |event: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                menu(ix, event.position, window, cx);
            },
        )
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
                        .text_size(crate::theme::text(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(SharedString::from(column.name.clone())),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(4.))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .font_family(crate::theme::mono_font())
                        .text_size(crate::theme::text(9.5))
                        .text_color(theme.t3)
                        .child(SharedString::from(
                            column.type_name.clone().unwrap_or_default().to_lowercase(),
                        ))
                        .when_some(link, |el, table| {
                            el.child(
                                div()
                                    .text_color(theme.acc)
                                    .child(SharedString::from(format!("→ {table}"))),
                            )
                        }),
                ),
        )
        .child(
            div()
                .text_size(crate::theme::text(8.))
                .text_color(theme.acc)
                .child(arrow),
        )
        .child(
            div()
                .id(("resize", data))
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
                            autofit(data, window, cx);
                        } else {
                            resize(data, event.position.x, width, window, cx);
                        }
                    },
                )
                .on_click(|_, _, cx| cx.stop_propagation()),
        )
}

/// What a cell shows: the fetched value, a staged new one, or (in a row being
/// added) nothing yet, so the database's default.
enum Shown {
    Value(Value),
    Edited(Value),
    Default,
}

/// Which columns a row shows, and how the pinned ones sit.
struct RowLayout {
    /// Data columns in display order.
    shown: Arc<Vec<usize>>,
    pinned: usize,
    pinned_width: f32,
    /// How far the grid is scrolled sideways.
    shift: f32,
}

#[derive(Clone, Copy)]
struct RowState {
    new: bool,
    deleted: bool,
}

#[allow(clippy::too_many_arguments)]
fn render_row(
    ix: usize,
    row: &[Shown],
    state: RowState,
    layout: &Arc<RowLayout>,
    widths: &Arc<Vec<f32>>,
    links: &Arc<Vec<bool>>,
    selection: Option<Selection>,
    theme: &Theme,
    callbacks: &Rc<Callbacks>,
    origin_x: &Rc<Cell<Pixels>>,
) -> AnyElement {
    let row_selected = selection.is_some_and(|s| s.rows().contains(&ix));
    let background = if ix % 2 == 1 { Some(theme.s2) } else { None };
    let select = callbacks.select.clone();
    let edit = callbacks.edit.clone();
    let follow = callbacks.follow.clone();
    let (origin_x, column_widths) = (origin_x.clone(), widths.clone());
    let column_links = links.clone();
    let linked = |column: usize| !state.new && links.get(column).copied().unwrap_or(false);
    let click_layout = layout.clone();
    let stripe = background.unwrap_or(theme.s1);
    let index_cell = || {
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
            .text_color(if state.new {
                theme.ok
            } else if row_selected {
                theme.acc
            } else {
                theme.t3
            })
            .child(if state.new {
                SharedString::new_static("+")
            } else {
                SharedString::from((ix + 1).to_string())
            })
    };
    // The cell at display column `column`.
    let cell_at = |column: usize| {
        let data = layout.shown[column];
        let width = widths[column];
        let selected = selection.is_some_and(|s| s.contains(ix, column));
        let focused = selection.is_some_and(|s| s.head == (ix, column));
        let link = linked(data);
        let el = match &row[data] {
            Shown::Value(value) => cell(value, width, link, theme),
            // A staged change: amber, like a pending edit anywhere.
            Shown::Edited(value) => cell(value, width, link, theme).when(!state.new, |el| {
                el.bg(Rgba {
                    a: 0.14,
                    ..theme.warn
                })
            }),
            Shown::Default => default_cell(width, theme),
        };
        el.when(state.deleted, |el| el.line_through().text_color(theme.t3))
            .when(selected, |el| el.bg(theme.acc_wash))
            .when(focused, |el| {
                // An outline drawn over the cell, not a border: nothing moves.
                el.relative().child(
                    div()
                        .absolute()
                        .inset_0()
                        .border_1()
                        .border_color(theme.acc),
                )
            })
    };

    div()
        .id(ix)
        .h(px(ROW_HEIGHT))
        .relative()
        .flex()
        .flex_row()
        .font_family(crate::theme::mono_font())
        .text_size(crate::theme::text(11.5))
        .border_b_1()
        .border_color(theme.line_2)
        .when_some(background, |el, bg| el.bg(bg))
        .when(state.new, |el| {
            el.bg(Rgba {
                a: 0.08,
                ..theme.ok
            })
        })
        .when(state.deleted, |el| {
            el.bg(Rgba {
                a: 0.10,
                ..theme.err
            })
        })
        .hover(|style| style.bg(theme.s3))
        // One handler per row: the column comes from where the click landed.
        .on_mouse_down(
            MouseButton::Left,
            move |event: &MouseDownEvent, window, cx| {
                let layout = &click_layout;
                let x = f32::from(event.position.x - origin_x.get()) - INDEX_WIDTH;
                // Over the pinned columns, which stay put as the rest scroll.
                let in_view = x - layout.shift;
                let hit = if layout.shift > 0. && in_view >= 0. && in_view < layout.pinned_width {
                    column_at(&column_widths[..layout.pinned], in_view)
                } else {
                    column_at(&column_widths, x)
                };
                if let Some((column, offset)) = hit {
                    let data = layout.shown[column];
                    let on_arrow = !state.new
                        && column_links.get(data).copied().unwrap_or(false)
                        && offset < LINK_ZONE;
                    if on_arrow && event.click_count == 1 {
                        select(ix, column, false, window, cx);
                        follow(ix, data, window, cx);
                    } else if event.click_count >= 2 {
                        edit(ix, data, window, cx);
                    } else {
                        select(ix, column, event.modifiers.shift, window, cx);
                    }
                }
            },
        )
        .child(index_cell())
        .children((0..layout.shown.len()).map(&cell_at))
        .child(div().flex_1())
        // Scrolled sideways: the row number and pinned cells stay in view.
        .when(layout.shift > 0., |el| {
            el.child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(layout.shift))
                    .flex()
                    .bg(stripe)
                    .when(state.new, |el| {
                        el.bg(Rgba {
                            a: 0.08,
                            ..theme.ok
                        })
                    })
                    .child(index_cell())
                    .children((0..layout.pinned).map(&cell_at))
                    .when(layout.pinned > 0, |el| {
                        el.border_r_2().border_color(theme.line)
                    }),
            )
        })
        .child(div().flex_1())
        .into_any_element()
}

/// The column under `x` (pixels from the first data column's left edge), and
/// how far into it `x` is.
fn column_at(widths: &[f32], x: f32) -> Option<(usize, f32)> {
    if x < 0. {
        return None;
    }
    let mut left = 0.;
    for (column, width) in widths.iter().enumerate() {
        if x < left + width {
            return Some((column, x - left));
        }
        left += width;
    }
    None
}

/// How far into a foreign-key cell a click follows the link: the arrow and its padding.
const LINK_ZONE: f32 = 28.;

/// One element per cell. Text is cut to what fits the column up front, so the
/// grid never measures text for an ellipsis while it draws.
fn cell(value: &Value, width: f32, link: bool, theme: &Theme) -> gpui::Div {
    let el = div()
        .w(px(width))
        .h_full()
        .flex_none()
        .px(px(10.))
        .flex()
        .items_center()
        .when(value.is_numeric(), |el| el.justify_end())
        .border_r_1()
        .border_color(theme.line_2)
        .overflow_hidden()
        .whitespace_nowrap();
    if value.is_null() {
        return el
            .text_color(theme.t3)
            .italic()
            .child(SharedString::new_static("NULL"));
    }
    let display = value.display();
    // JSON objects and Postgres arrays read as structure, so they're dimmed.
    let structured = matches!(value, Value::Text(_))
        && matches!(
            (display.chars().next(), display.chars().last()),
            (Some('{'), Some('}')) | (Some('['), Some(']'))
        );
    // A foreign key: an arrow to the row it points at, before the value.
    if link {
        let numeric = value.is_numeric();
        return el
            .justify_start()
            .gap(px(6.))
            .child(
                svg()
                    .path(assets::icon("arrow-up-right"))
                    .size(px(12.))
                    .flex_none()
                    .text_color(theme.acc),
            )
            .when(numeric, |el| el.child(div().flex_1()))
            .child(SharedString::from(fit(&display, width - 18.)));
    }
    el.when(structured, |el| el.text_color(theme.t2))
        .child(SharedString::from(fit(&display, width)))
}

/// A cell in a row being added that has no value yet: the database fills it in.
fn default_cell(width: f32, theme: &Theme) -> gpui::Div {
    div()
        .w(px(width))
        .h_full()
        .flex_none()
        .px(px(10.))
        .flex()
        .items_center()
        .border_r_1()
        .border_color(theme.line_2)
        .text_color(theme.t3)
        .italic()
        .child(SharedString::new_static("default"))
}

/// Shorten `text` to the characters that fit a column `width` wide, ending
/// with "…" when cut. Line breaks are shown as ⏎.
fn fit(text: &str, width: f32) -> String {
    let budget = (((width - 20.) / MONO_ADVANCE).floor() as usize).clamp(1, CELL_CHARS);
    let mut out = String::with_capacity(budget.min(text.len()) + 3);
    for (count, c) in text.chars().enumerate() {
        if count == budget {
            out.pop();
            out.push('…');
            break;
        }
        out.push(if c == '\n' { '⏎' } else { c });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid_of(names: &[&str]) -> Grid {
        Grid::new(ResultSet {
            columns: names
                .iter()
                .map(|n| Column {
                    name: n.to_string(),
                    type_name: None,
                })
                .collect(),
            rows: vec![
                names
                    .iter()
                    .enumerate()
                    .map(|(i, _)| Value::Integer(i as i64))
                    .collect(),
            ],
            ..Default::default()
        })
    }

    #[test]
    fn columns_hide_move_and_pin() {
        let mut grid = grid_of(&["id", "name", "email", "total"]);
        grid.hide(1);
        assert_eq!(grid.columns(), [0, 2, 3]);
        assert_eq!(grid.hidden(), 1);
        // Pin `total`: it moves to the left, in front of the rest.
        assert_eq!(grid.toggle_pin(2), 0);
        assert_eq!((grid.columns(), grid.pinned), (&[3, 0, 2][..], 1));
        // Moving keeps pinned and unpinned apart.
        assert_eq!(grid.move_column(1, false), None);
        assert_eq!(grid.move_column(1, true), Some(2));
        assert_eq!(grid.columns(), [3, 2, 0]);
        // Display positions map to data columns.
        assert_eq!(grid.cell(0, 0), &Value::Integer(3));
        assert_eq!(grid.display_of(0), Some(2));
        assert_eq!(grid.display_of(1), None);
        // Unpin: back to just after the pinned ones (none now).
        assert_eq!(grid.toggle_pin(0), 0);
        assert_eq!(grid.pinned, 0);
        // The last column can't be hidden.
        let mut one = grid_of(&["id"]);
        one.hide(0);
        assert_eq!(one.columns(), [0]);
    }

    #[test]
    fn layouts_are_kept_by_name_through_schema_changes() {
        let mut grid = grid_of(&["id", "name", "email", "total"]);
        grid.hide(1);
        grid.toggle_pin(2);
        let layout = grid.layout();
        assert_eq!(layout.shown, ["total", "id", "email"]);
        assert_eq!(layout.hidden, ["name"]);
        // `email` dropped, `plan` added since: the rest keep their places, the
        // new column shows at the end, `name` stays hidden.
        let mut later = grid_of(&["id", "name", "total", "plan"]);
        later.apply_layout(&layout);
        let names: Vec<_> = later
            .columns()
            .iter()
            .map(|&c| later.data.columns[c].name.as_str())
            .collect();
        assert_eq!(names, ["total", "id", "plan"]);
        assert_eq!(later.pinned, 1);
    }

    #[test]
    fn fit_cuts_to_the_column_with_an_ellipsis() {
        // 20px of padding, 6.9px per character.
        let width = 20. + 6.9 * 5.;
        assert_eq!(fit("short", width), "short");
        assert_eq!(fit("longer text", width), "long…");
        assert_eq!(fit("a\nb", width), "a⏎b");
    }

    #[test]
    fn column_at_maps_x_to_columns() {
        let widths = [100., 50., 80.];
        assert_eq!(column_at(&widths, -1.), None);
        assert_eq!(column_at(&widths, 0.), Some((0, 0.)));
        assert_eq!(column_at(&widths, 149.), Some((1, 49.)));
        assert_eq!(column_at(&widths, 229.), Some((2, 79.)));
        assert_eq!(column_at(&widths, 400.), None);
    }

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
