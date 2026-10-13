//! The main window: title bar, icon rail, explorer, workspace, inspector, status bar.
//! Layout follows PLAN.md, "Design direction".

use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, Context, Entity, ExternalPaths, FocusHandle, FontWeight, IntoElement,
    PathPromptOptions, Render, Rgba, SharedString, Subscription, Task, Window, div, prelude::*, px,
};
use gpui_component::input::{Editor, EditorState};
use xenon_driver::{CancelHandle, Connection, ObjectKind, QueryOutcome, QueryResult, Schema};
use xenon_sqlite::SqliteConnection;

use crate::results::{self, Grid};
use crate::theme::{self, ActiveTheme, MONO_FONT, Mode, Theme, UI_FONT};
use crate::{CancelQuery, OpenFile, RunQuery, ToggleExplorer, ToggleInspector};

const TITLEBAR_HEIGHT: f32 = 34.;
const STATUSBAR_HEIGHT: f32 = 24.;
const RAIL_WIDTH: f32 = 44.;
const EXPLORER_WIDTH: f32 = 230.;
const INSPECTOR_WIDTH: f32 = 260.;
const EDITOR_HEIGHT: f32 = 220.;
/// Room for the macOS traffic lights in the transparent title bar.
const TRAFFIC_LIGHT_INSET: f32 = 78.;
/// Rows kept from a single result set; the rest are dropped and flagged.
const ROW_LIMIT: usize = 100_000;
/// Rows fetched when a table is opened from the explorer.
const BROWSE_LIMIT: usize = 1_000;

struct Session {
    conn: Arc<dyn Connection>,
    version: String,
    schemas: Vec<Schema>,
}

enum Query {
    Idle,
    Running {
        cancel: Arc<dyn CancelHandle>,
        _task: Task<()>,
    },
    Failed(SharedString),
    Done(Arc<QueryResult>),
}

pub struct Workspace {
    focus_handle: FocusHandle,
    editor: Entity<EditorState>,
    session: Option<Session>,
    /// A problem opening a database, shown in the explorer.
    notice: Option<SharedString>,
    query: Query,
    grid: Option<Grid>,
    selected_row: Option<usize>,
    expanded: HashSet<String>,
    show_explorer: bool,
    show_inspector: bool,
    _appearance: Subscription,
}

impl Workspace {
    pub fn new(path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .placeholder("-- write SQL here, ⌘↵ to run")
        });
        // Deferred so it runs after gpui-component's Root has been built around us.
        cx.defer_in(window, |this: &mut Self, window, cx| {
            this.editor.update(cx, |editor, cx| editor.focus(window, cx));
        });

        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            theme::apply(Mode::from_appearance(window.appearance()), cx);
            cx.notify();
        });

        let mut this = Self {
            focus_handle,
            editor,
            session: None,
            notice: None,
            query: Query::Idle,
            grid: None,
            selected_row: None,
            expanded: HashSet::new(),
            show_explorer: true,
            show_inspector: true,
            _appearance: appearance,
        };
        if let Some(path) = path {
            this.open_path(path, cx);
        }
        this
    }

    pub fn open_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.notice = None;
        let task = cx.background_spawn(async move {
            let conn = SqliteConnection::open(&path)?;
            let schemas = conn.introspect()?;
            anyhow::Ok((conn, schemas))
        });
        cx.spawn(async move |this, cx| {
            let opened = task.await;
            this.update(cx, |this, cx| {
                match opened {
                    Ok((conn, schemas)) => {
                        this.session = Some(Session {
                            version: conn.server_version(),
                            conn: Arc::new(conn),
                            schemas,
                        });
                        this.query = Query::Idle;
                        this.grid = None;
                        this.selected_row = None;
                        this.expanded.clear();
                    }
                    Err(err) => this.notice = Some(format!("{err:#}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn open_file(&mut self, _: &OpenFile, _: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await
                && let Some(path) = paths.into_iter().next()
            {
                this.update(cx, |this, cx| this.open_path(path, cx)).ok();
            }
        })
        .detach();
    }

    fn run_query(&mut self, _: &RunQuery, _: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            self.notice = Some("Open a database first (⌘O).".into());
            cx.notify();
            return;
        };
        if matches!(self.query, Query::Running { .. }) {
            return;
        }

        let editor = self.editor.read(cx);
        let selection = editor.selected_value();
        let sql = if selection.trim().is_empty() {
            editor.value().to_string()
        } else {
            selection.to_string()
        };
        if sql.trim().is_empty() {
            return;
        }

        let conn = session.conn.clone();
        let cancel = conn.cancel_handle();
        let work = cx.background_spawn(async move {
            let result = conn.execute(&sql, ROW_LIMIT);
            // Statements without rows may have changed the schema.
            let schemas = match &result {
                Ok(QueryResult {
                    outcome: QueryOutcome::Affected(_),
                    ..
                }) => conn.introspect().ok(),
                _ => None,
            };
            (result, schemas)
        });
        let task = cx.spawn(async move |this, cx| {
            let (result, schemas) = work.await;
            this.update(cx, |this, cx| this.finish_query(result, schemas, cx))
                .ok();
        });
        self.query = Query::Running {
            cancel,
            _task: task,
        };
        cx.notify();
    }

    fn finish_query(
        &mut self,
        result: anyhow::Result<QueryResult>,
        schemas: Option<Vec<Schema>>,
        cx: &mut Context<Self>,
    ) {
        if let (Some(session), Some(schemas)) = (&mut self.session, schemas) {
            session.schemas = schemas;
        }
        self.selected_row = None;
        match result {
            Ok(mut result) => {
                if let QueryOutcome::Rows(rows) = &mut result.outcome {
                    self.grid = Some(Grid::new(std::mem::take(rows)));
                    result.outcome = QueryOutcome::Rows(Default::default());
                } else {
                    self.grid = None;
                }
                self.query = Query::Done(Arc::new(result));
            }
            Err(err) => {
                self.grid = None;
                self.query = Query::Failed(format!("{err:#}").into());
            }
        }
        cx.notify();
    }

    fn cancel_query(&mut self, _: &CancelQuery, _: &mut Window, _: &mut Context<Self>) {
        if let Query::Running { cancel, .. } = &self.query {
            cancel.cancel();
        }
    }

    fn browse(&mut self, table: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.session else { return };
        let sql = format!(
            "select * from {} limit {BROWSE_LIMIT};",
            session.conn.quote_ident(table)
        );
        self.editor
            .update(cx, |editor, cx| editor.set_value(sql, window, cx));
        self.run_query(&RunQuery, window, cx);
    }

    fn toggle_expanded(&mut self, name: &str, cx: &mut Context<Self>) {
        if !self.expanded.remove(name) {
            self.expanded.insert(name.to_string());
        }
        cx.notify();
    }

    fn toggle_explorer(&mut self, _: &ToggleExplorer, _: &mut Window, cx: &mut Context<Self>) {
        self.show_explorer = !self.show_explorer;
        cx.notify();
    }

    fn toggle_inspector(&mut self, _: &ToggleInspector, _: &mut Window, cx: &mut Context<Self>) {
        self.show_inspector = !self.show_inspector;
        cx.notify();
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();

        let body = div()
            .flex()
            .flex_row()
            .flex_1()
            .min_h(px(0.))
            .child(rail(&theme))
            .when(self.show_explorer, |el| {
                el.child(self.render_explorer(&theme, cx))
            })
            .child(self.render_center(&theme, cx))
            .when(self.show_inspector, |el| {
                el.child(self.render_inspector(&theme))
            });

        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::open_file))
            .on_action(cx.listener(Self::run_query))
            .on_action(cx.listener(Self::cancel_query))
            .on_action(cx.listener(Self::toggle_explorer))
            .on_action(cx.listener(Self::toggle_inspector))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                if let Some(path) = paths.0.first() {
                    this.open_path(path.clone(), cx);
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.s1)
            .font_family(UI_FONT)
            .text_color(theme.text)
            .text_size(px(12.))
            .child(self.render_titlebar(&theme))
            .child(body)
            .child(self.render_statusbar(&theme))
    }
}

impl Workspace {
    fn render_titlebar(&self, theme: &Theme) -> impl IntoElement {
        let tab = match &self.session {
            Some(session) => SharedString::from(session.conn.name().to_string()),
            None => "Welcome".into(),
        };

        div()
            .h(px(TITLEBAR_HEIGHT))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .bg(theme.bg)
            .border_b_1()
            .border_color(theme.line)
            .child(
                div()
                    .w(px(TRAFFIC_LIGHT_INSET))
                    .h_full()
                    .border_r_1()
                    .border_color(theme.line),
            )
            .child(
                div()
                    .h_full()
                    .px(px(14.))
                    .flex()
                    .items_center()
                    .bg(theme.s1)
                    .relative()
                    .border_r_1()
                    .border_color(theme.line)
                    .child(tab)
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom_0()
                            .h(px(1.))
                            .bg(theme.acc),
                    ),
            )
            .child(div().flex_1())
            .child(
                div()
                    .mr(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .pl(px(10.))
                    .pr(px(5.))
                    .py(px(3.))
                    .bg(theme.s1)
                    .border_1()
                    .border_color(theme.line)
                    .text_color(theme.t3)
                    .text_size(px(11.5))
                    .child("Go to anything")
                    .child(kbd(theme, "⌘K")),
            )
    }

    fn render_explorer(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let mut list = div()
            .id("explorer-list")
            .flex_1()
            .overflow_y_scroll()
            .pb(px(8.));

        match &self.session {
            None => {
                list = list.child(empty_hint(theme, "No connection")).child(
                    div()
                        .id("open-file")
                        .mx(px(12.))
                        .mt(px(4.))
                        .px(px(10.))
                        .py(px(6.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .border_1()
                        .border_color(theme.line)
                        .text_color(theme.t2)
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme.text).border_color(theme.t3))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_file(&OpenFile, window, cx)
                        }))
                        .child("Open SQLite file")
                        .child(kbd(theme, "⌘O")),
                );
                list = list.child(empty_hint(theme, "or drop a database file here"));
            }
            Some(session) => {
                list = list.child(
                    tree_row(0)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(div().w(px(12.)).child("▾"))
                        .child(div().flex_1().truncate().child(session.conn.name().to_string()))
                        .child(env_tag("LOCAL", theme.t3)),
                );
                for schema in &session.schemas {
                    for object in &schema.objects {
                        list = list.children(self.render_object(object, theme, cx));
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

        div()
            .w(px(EXPLORER_WIDTH))
            .flex_none()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme.line)
            .child(panel_label(theme, "Explorer"))
            .child(list)
    }

    fn render_object(
        &self,
        object: &xenon_driver::SchemaObject,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let expanded = self.expanded.contains(&object.name);
        let toggle_name = object.name.clone();
        let browse_name = object.name.clone();
        let icon = match object.kind {
            ObjectKind::Table => "▦",
            ObjectKind::View => "◇",
        };

        let mut rows = vec![
            tree_row(1)
                .id(SharedString::from(format!("obj-{}", object.name)))
                .cursor_pointer()
                .hover(|s| s.bg(theme.s3).text_color(theme.text))
                .text_color(theme.t2)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.browse(&browse_name, window, cx)
                }))
                .child(
                    div()
                        .id(SharedString::from(format!("chev-{}", object.name)))
                        .w(px(12.))
                        .text_color(theme.t3)
                        .hover(|s| s.text_color(theme.text))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_expanded(&toggle_name, cx)
                        }))
                        .child(if expanded { "▾" } else { "▸" }),
                )
                .child(div().text_color(theme.t3).child(icon))
                .child(div().flex_1().truncate().child(object.name.clone()))
                .child(
                    div()
                        .font_family(MONO_FONT)
                        .text_size(px(10.5))
                        .text_color(theme.t3)
                        .child(object.columns.len().to_string()),
                )
                .into_any_element(),
        ];

        if expanded {
            rows.extend(object.columns.iter().map(|column| {
                tree_row(2)
                    .text_color(theme.t2)
                    .child(div().flex_1().truncate().child(column.name.clone()))
                    .when(column.primary_key, |el| {
                        el.child(env_tag("PK", theme.acc))
                    })
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

    fn render_center(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let editor = div()
            .h(px(EDITOR_HEIGHT))
            .flex_none()
            .bg(theme.s2)
            .border_b_1()
            .border_color(theme.line)
            .font_family(MONO_FONT)
            .text_size(px(12.5))
            .py(px(6.))
            .child(Editor::new(&self.editor).h_full().appearance(false));

        div()
            .flex_1()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .child(editor)
            .child(self.render_results_bar(theme))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .child(self.render_results(theme, cx)),
            )
    }

    fn render_results_bar(&self, theme: &Theme) -> impl IntoElement {
        let mut cells: Vec<(SharedString, Rgba)> = Vec::new();
        match &self.query {
            Query::Idle => cells.push(("No results".into(), theme.t3)),
            Query::Running { .. } => cells.push(("Running… ⌘. to cancel".into(), theme.t2)),
            Query::Failed(_) => cells.push(("ERROR".into(), theme.err)),
            Query::Done(result) => {
                cells.push(("OK".into(), theme.ok));
                match (&result.outcome, &self.grid) {
                    (QueryOutcome::Rows(_), Some(grid)) => {
                        let count = grid.data.rows.len();
                        let label = if grid.data.truncated {
                            format!("first {} rows", group_digits(count))
                        } else {
                            format!("{} rows", group_digits(count))
                        };
                        cells.push((label.into(), theme.t2));
                    }
                    (QueryOutcome::Affected(n), _) => {
                        cells.push((format!("{} rows affected", group_digits(*n as usize)).into(), theme.t2))
                    }
                    _ => {}
                }
                cells.push((format_duration(result.elapsed).into(), theme.t3));
                if result.statements > 1 {
                    cells.push((format!("{} statements", result.statements).into(), theme.t3));
                }
            }
        }

        div()
            .h(px(30.))
            .flex_none()
            .flex()
            .flex_row()
            .border_b_1()
            .border_color(theme.line)
            .font_family(MONO_FONT)
            .text_size(px(11.))
            .children(cells.into_iter().map(|(text, color)| {
                div()
                    .h_full()
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .border_r_1()
                    .border_color(theme.line)
                    .text_color(color)
                    .child(text)
            }))
    }

    fn render_results(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        if let Some(grid) = &self.grid {
            if grid.data.rows.is_empty() && grid.data.columns.is_empty() {
                return centered_hint(theme, "The query returned no columns.");
            }
            let this = cx.entity().downgrade();
            let on_select: results::OnSelect = Rc::new(move |ix, _, cx| {
                this.update(cx, |this, cx| {
                    this.selected_row = Some(ix);
                    cx.notify();
                })
                .ok();
            });
            return results::render(grid, self.selected_row, theme, on_select).into_any_element();
        }

        match &self.query {
            Query::Failed(message) => div()
                .m(px(12.))
                .p(px(12.))
                .bg(theme.s1)
                .border_l_2()
                .border_color(theme.err)
                .font_family(MONO_FONT)
                .text_size(px(11.5))
                .text_color(theme.t2)
                .child(message.clone())
                .into_any_element(),
            Query::Running { .. } => centered_hint(theme, "Running…"),
            Query::Done(_) => centered_hint(theme, "Done."),
            Query::Idle if self.session.is_some() => {
                centered_hint(theme, "Run a query with ⌘↵, or click a table to browse it.")
            }
            Query::Idle => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .child(
                    div()
                        .text_size(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Xenon"),
                )
                .child(
                    div()
                        .text_color(theme.t3)
                        .child("Open a SQLite file with ⌘O, or drop one on the window."),
                )
                .into_any_element(),
        }
    }

    fn render_inspector(&self, theme: &Theme) -> impl IntoElement {
        let mut panel = div()
            .w(px(INSPECTOR_WIDTH))
            .flex_none()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(theme.line);

        let selected = self
            .selected_row
            .zip(self.grid.as_ref())
            .and_then(|(ix, grid)| grid.data.rows.get(ix).map(|row| (ix, row, &grid.data.columns)));

        let Some((ix, row, columns)) = selected else {
            return panel
                .child(panel_label(theme, "Inspector"))
                .child(empty_hint(theme, "Select a row to see every field"));
        };

        panel = panel.child(panel_label(theme, &format!("Row {}", ix + 1)));
        let fields = div()
            .id("inspector-fields")
            .flex_1()
            .overflow_y_scroll()
            .children(columns.iter().zip(row).map(|(column, value)| {
                div()
                    .px(px(14.))
                    .py(px(6.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .border_b_1()
                    .border_color(theme.line_2)
                    .font_family(MONO_FONT)
                    .text_size(px(11.))
                    .child(
                        div()
                            .text_color(theme.t3)
                            .child(SharedString::from(column.name.clone())),
                    )
                    .child(if value.is_null() {
                        div().italic().text_color(theme.t3).child("NULL")
                    } else {
                        div().child(SharedString::from(value.display().into_owned()))
                    })
            }));
        panel.child(fields)
    }

    fn render_statusbar(&self, theme: &Theme) -> impl IntoElement {
        let cell = |text: SharedString, color: Rgba| {
            div()
                .h_full()
                .px(px(12.))
                .flex()
                .items_center()
                .border_r_1()
                .border_color(theme.line)
                .text_color(color)
                .child(text)
        };

        let mut bar = div()
            .h(px(STATUSBAR_HEIGHT))
            .flex_none()
            .flex()
            .flex_row()
            .bg(theme.bg)
            .border_t_1()
            .border_color(theme.line)
            .font_family(MONO_FONT)
            .text_size(px(10.5));

        bar = match &self.session {
            Some(session) => bar
                .child(
                    cell("".into(), theme.t2)
                        .gap(px(6.))
                        .child(div().text_color(theme.ok).child("●"))
                        .child(session.conn.name().to_string()),
                )
                .child(cell(session.version.clone().into(), theme.t3))
                .child(cell("auto-commit".into(), theme.t3)),
            None => bar.child(cell("no connection".into(), theme.t3)),
        };

        bar.child(div().flex_1()).child(
            cell(concat!("xenon ", env!("CARGO_PKG_VERSION")).into(), theme.t3).border_l_1(),
        )
    }
}

fn rail(theme: &Theme) -> impl IntoElement {
    let icon = |glyph: &'static str, active: bool| {
        div()
            .size(px(30.))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(13.))
            .text_color(if active { theme.text } else { theme.t3 })
            .when(active, |el| el.border_l_2().border_color(theme.acc))
            .child(glyph)
    };

    div()
        .w(px(RAIL_WIDTH))
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .pt(px(8.))
        .gap(px(2.))
        .bg(theme.bg)
        .border_r_1()
        .border_color(theme.line)
        .child(icon("▤", true))
        .child(icon("⌕", false))
        .child(icon("◷", false))
        .child(icon("★", false))
        .child(icon("⚙", false))
}

fn tree_row(depth: usize) -> gpui::Div {
    div()
        .h(px(24.))
        .pl(px(12. + depth as f32 * 12.))
        .pr(px(12.))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.))
}

fn env_tag(label: &'static str, color: Rgba) -> impl IntoElement {
    div()
        .px(px(4.))
        .border_1()
        .border_color(color)
        .text_color(color)
        .font_family(MONO_FONT)
        .text_size(px(9.))
        .font_weight(FontWeight::NORMAL)
        .child(label)
}

fn panel_label(theme: &Theme, text: &str) -> AnyElement {
    div()
        .px(px(12.))
        .pt(px(10.))
        .pb(px(6.))
        .font_family(MONO_FONT)
        .text_size(px(10.))
        .text_color(theme.t3)
        .child(text.to_uppercase())
        .into_any_element()
}

fn empty_hint(theme: &Theme, text: &'static str) -> AnyElement {
    div()
        .px(px(12.))
        .py(px(6.))
        .text_color(theme.t3)
        .child(text)
        .into_any_element()
}

fn centered_hint(theme: &Theme, text: &'static str) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_color(theme.t3)
        .child(text)
        .into_any_element()
}

fn kbd(theme: &Theme, keys: &'static str) -> impl IntoElement {
    div()
        .px(px(5.))
        .bg(theme.s2)
        .border_1()
        .border_color(theme.line)
        .font_family(MONO_FONT)
        .text_size(px(10.))
        .text_color(theme.t2)
        .child(keys)
}

fn format_duration(elapsed: Duration) -> String {
    let ms = elapsed.as_secs_f64() * 1000.;
    if ms < 1. {
        format!("{:.2} ms", ms)
    } else if ms < 1000. {
        format!("{:.1} ms", ms)
    } else {
        format!("{:.2} s", ms / 1000.)
    }
}

fn group_digits(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_thousands() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1000), "1,000");
        assert_eq!(group_digits(1234567), "1,234,567");
    }

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration(Duration::from_micros(250)), "0.25 ms");
        assert_eq!(format_duration(Duration::from_micros(38_200)), "38.2 ms");
        assert_eq!(format_duration(Duration::from_millis(2500)), "2.50 s");
    }
}
