//! The main window: title bar with query tabs, icon rail, side panel (explorer
//! or history), editor and results, inspector, status bar.
//! Layout follows PLAN.md, "Design direction".

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, ClickEvent, ClipboardItem, Context, Entity, ExternalPaths, FocusHandle,
    FontWeight, IntoElement, MouseButton, MouseMoveEvent, PathPromptOptions, Pixels, Render, Rgba,
    ScrollStrategy, SharedString, Subscription, Task, Window, div, prelude::*, px, svg,
};
use gpui_component::input::{Editor, EditorState, InputEvent, InputState};
use xenon_driver::export::{self, Format};
use xenon_driver::sql::{Risk, SortDirection, assess, browse_table, statement_at};
use xenon_driver::{
    CancelHandle, Connection, ObjectKind, QueryError, QueryOutcome, QueryResult, Schema,
};
use xenon_driver::{ConnectOptions, SslMode};
use xenon_sqlite::SqliteConnection;
use xenon_store::{
    Environment, HistoryEntry, NewHistoryEntry, SavedConnection, ServerEngine, Store, Target,
    secrets,
};

use crate::assets;
use crate::confirm::{ConfirmDialog, ConfirmEvent};
use crate::connect::{ConnectDialog, ConnectEvent, Request, RequestTarget, Subject};
use crate::detect::{Detected, detect};
use crate::discover::{Found, FoundTarget};
use crate::palette::{self, Palette, PaletteEvent};
use crate::results::{self, Grid, Selection};
use crate::servers;
use crate::theme::{self, ActiveTheme, MONO_FONT, Mode, Theme, UI_FONT};
use crate::widgets::{
    self as widgets, ButtonKind, Engine, RADIUS_CONTROL, centered_hint, connection_tags,
    empty_hint, env_tag, field, format_duration, group_digits, kbd, kbd_on_accent, panel_label,
    pretty_json,
};
use crate::{
    CancelQuery, CloseConnection, CloseTab, CopyCsv, CopyJson, CopySelection, Explain, ExtendDown,
    ExtendLeft, ExtendRight, ExtendUp, FocusEditor, MoveDown, MoveLeft, MoveRight, MoveUp,
    NewConnection, NewTab, NextTab, OpenFile, OpenPalette, PrevTab, Quit, RunAll, RunQuery,
    SelectAllCells, ShowConnections, ShowExplorer, ShowHistory, ToggleExplorer, ToggleInspector,
    UseDarkTheme, UseLightTheme, UseSystemTheme,
};

const TITLEBAR_HEIGHT: f32 = 34.;
const STATUSBAR_HEIGHT: f32 = 24.;
const RAIL_WIDTH: f32 = 48.;
const SIDE_PANEL_WIDTH: f32 = 240.;
const MIN_SIDE_PANEL_WIDTH: f32 = 180.;
const MAX_SIDE_PANEL_WIDTH: f32 = 520.;
const INSPECTOR_WIDTH: f32 = 260.;
const EDITOR_HEIGHT: f32 = 220.;
const MIN_EDITOR_HEIGHT: f32 = 72.;
/// Space always left for the results below a dragged editor.
const MIN_RESULTS_HEIGHT: f32 = 180.;
const TAB_MAX_WIDTH: f32 = 240.;
/// Room for the macOS traffic lights in the transparent title bar.
const TRAFFIC_LIGHT_INSET: f32 = 78.;
/// Rows kept from a single result set; the rest are dropped and flagged.
const ROW_LIMIT: usize = 100_000;
/// Rows fetched when a table is opened from the explorer.
const BROWSE_LIMIT: usize = 1_000;

/// What to open on launch, from the command line.
pub enum Launch {
    Sqlite(PathBuf),
    /// A server URL, e.g. `postgres://…` or `mysql://…`.
    Url(ServerEngine, String),
}

impl Launch {
    pub fn from_arg(arg: String) -> Self {
        match detect(&arg) {
            Detected::Server(engine, url) => Launch::Url(engine, url),
            _ => Launch::Sqlite(arg.into()),
        }
    }
}

/// An open connection. Several can be open at once; each tab belongs to one.
pub(crate) struct Session {
    pub id: usize,
    pub engine: Engine,
    pub conn: Arc<dyn Connection>,
    pub version: String,
    pub schemas: Vec<Schema>,
    /// The saved name, or a generated one like `acme@localhost`.
    pub name: String,
    pub environment: Environment,
    pub read_only: bool,
    pub saved_id: Option<i64>,
    /// Explorer state: tables showing their columns (`schema.table`), folded
    /// schemas, and whether the whole connection is folded.
    pub expanded: HashSet<String>,
    pub collapsed_schemas: HashSet<String>,
    pub collapsed: bool,
}

/// Everything needed to open (and maybe remember) a connection.
struct Profile {
    saved_id: Option<i64>,
    /// Remember this connection after it opens.
    save: bool,
    name: String,
    environment: Environment,
    read_only: bool,
    target: RequestTarget,
}

impl Profile {
    fn from_saved(saved: &SavedConnection) -> Self {
        let target = match &saved.target {
            Target::Sqlite { path } => RequestTarget::Sqlite(path.clone()),
            Target::Server {
                engine,
                host,
                port,
                database,
                user,
                ssl,
                ssh,
            } => RequestTarget::Server(
                *engine,
                ConnectOptions {
                    host: host.clone(),
                    port: *port,
                    database: database.clone(),
                    user: user.clone(),
                    // Passwords are filled from the Keychain when connecting.
                    password: String::new(),
                    ssl: SslMode::parse(ssl).unwrap_or_default(),
                    read_only: saved.read_only,
                    ssh: ssh.as_ref().map(servers::ssh_options),
                },
            ),
        };
        Self {
            saved_id: Some(saved.id),
            save: true,
            name: saved.name.clone(),
            environment: saved.environment,
            read_only: saved.read_only,
            target,
        }
    }

    fn from_request(request: Request) -> Self {
        Self {
            saved_id: (request.id != 0).then_some(request.id),
            save: request.save,
            name: request.name,
            environment: request.environment,
            read_only: request.read_only,
            target: request.target,
        }
    }

    /// Runs on a background thread.
    fn connect(&self) -> anyhow::Result<Arc<dyn Connection>> {
        Ok(match &self.target {
            RequestTarget::Sqlite(path) => {
                Arc::new(SqliteConnection::open(Path::new(path), self.read_only)?)
            }
            RequestTarget::Server(engine, options) => {
                let mut options = options.clone();
                options.read_only = self.read_only;
                if let Some(id) = self.saved_id {
                    if options.password.is_empty() {
                        options.password =
                            secrets::get(&SavedConnection::secret_key(id))?.unwrap_or_default();
                    }
                    if let Some(ssh) = &mut options.ssh
                        && ssh.password.is_empty()
                    {
                        ssh.password =
                            secrets::get(&SavedConnection::ssh_secret_key(id))?.unwrap_or_default();
                    }
                }
                servers::connect(*engine, &options)?
            }
        })
    }

    /// Save (or update) this connection and its password. Runs on a background thread.
    fn remember(&self, store: Option<&Store>) -> anyhow::Result<Option<i64>> {
        let (Some(store), true) = (store, self.save) else {
            return Ok(self.saved_id);
        };
        let target = match &self.target {
            RequestTarget::Sqlite(path) => Target::Sqlite { path: path.clone() },
            RequestTarget::Server(engine, options) => Target::Server {
                engine: *engine,
                host: options.host.clone(),
                port: options.port,
                database: options.database.clone(),
                user: options.user.clone(),
                ssl: options.ssl.as_str().to_string(),
                ssh: options.ssh.as_ref().map(servers::ssh_tunnel),
            },
        };
        let id = store.save_connection(&SavedConnection {
            id: self.saved_id.unwrap_or(0),
            name: self.name.clone(),
            target,
            environment: self.environment,
            read_only: self.read_only,
        })?;
        if let RequestTarget::Server(_, options) = &self.target {
            if !options.password.is_empty() {
                secrets::set(&SavedConnection::secret_key(id), &options.password)?;
            }
            if let Some(ssh) = &options.ssh
                && !ssh.password.is_empty()
            {
                secrets::set(&SavedConnection::ssh_secret_key(id), &ssh.password)?;
            }
        }
        Ok(Some(id))
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct FieldFocus {
    pub paste: bool,
    pub explorer: bool,
    pub history: bool,
    pub browse: bool,
}

fn is_local_host(host: &str) -> bool {
    matches!(host, "" | "localhost" | "127.0.0.1" | "::1")
}

/// What the paste field shows under itself as you type.
fn paste_note(text: &str) -> (SharedString, bool) {
    let note = match detect(text) {
        Detected::Server(engine, url) => match servers::parse_url(engine, &url) {
            Ok(o) => format!(
                "{} · {} · press Enter",
                engine.label(),
                servers::describe_options(engine, &o)
            ),
            Err(_) => format!("{} · incomplete URL", engine.label()),
        },
        Detected::Sqlite(path) => format!("SQLite file · {} · press Enter", path.display()),
        Detected::NotYet(engine) => {
            return (format!("{engine} · coming soon").into(), true);
        }
        Detected::Unknown => String::new(),
    };
    (note.into(), false)
}

/// An action waiting on the confirmation dialog.
enum Pending {
    Run {
        tab_id: usize,
        sql: String,
        origin: Option<usize>,
    },
    Forget(i64),
}

impl Session {
    /// One schema (SQLite's `main`, Postgres's `public`, the MySQL database you
    /// connected to) isn't worth a level of its own in the tree.
    pub fn shows_schemas(&self) -> bool {
        self.schemas.len() > 1
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunScope {
    /// The selection, or else the statement under the cursor.
    Statement,
    /// The selection, or else the whole editor.
    All,
    /// The plan for the selection or the statement under the cursor.
    Explain,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThemeChoice {
    System,
    Light,
    Dark,
}

impl ThemeChoice {
    const SETTING: &str = "theme";

    fn parse(value: &str) -> Self {
        match value {
            "light" => ThemeChoice::Light,
            "dark" => ThemeChoice::Dark,
            _ => ThemeChoice::System,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            ThemeChoice::System => "system",
            ThemeChoice::Light => "light",
            ThemeChoice::Dark => "dark",
        }
    }

    fn mode(self, window: &Window) -> Mode {
        match self {
            ThemeChoice::System => Mode::from_appearance(window.appearance()),
            ThemeChoice::Light => Mode::Light,
            ThemeChoice::Dark => Mode::Dark,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidePanel {
    Explorer,
    History,
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

/// A tab opened by browsing a table: its query is generated from a filter and
/// a sort, so header clicks sort on the server rather than in the grid.
struct Browse {
    schema: String,
    table: String,
    filter: Entity<InputState>,
    order: Option<(String, SortDirection)>,
    _filter_enter: Subscription,
}

impl Browse {
    fn key(&self) -> String {
        format!("{}.{}", self.schema, self.table)
    }
}

/// One query tab: its own editor and its own results.
pub(crate) struct Tab {
    id: usize,
    /// The connection this tab runs against; `None` shows the Connect screen.
    pub(crate) session: Option<usize>,
    title: SharedString,
    browse: Option<Browse>,
    pub(crate) editor: Entity<EditorState>,
    query: Query,
    grid: Option<Grid>,
    selection: Option<Selection>,
    /// Widths the user dragged, by column name; kept across re-runs.
    column_widths: HashMap<String, f32>,
    /// Byte offset in the editor where the last run's SQL began, to place
    /// error positions; `None` when the SQL ran was rewritten (Explain).
    run_origin: Option<usize>,
}

impl Tab {
    fn manual_widths(&self, grid: &Grid) -> Vec<Option<f32>> {
        grid.data
            .columns
            .iter()
            .map(|c| self.column_widths.get(&c.name).copied())
            .collect()
    }
}

/// A mouse drag in progress.
enum Drag {
    Column {
        tab_id: usize,
        column: String,
        start_x: Pixels,
        start_width: f32,
    },
    /// The divider between the editor and the results.
    Split { start_y: Pixels, start_height: f32 },
    /// The side panel's right edge.
    SidePanel { start_x: Pixels, start_width: f32 },
}

pub struct Workspace {
    focus_handle: FocusHandle,
    tabs: Vec<Tab>,
    active: usize,
    next_tab_id: usize,
    pub(crate) sessions: Vec<Session>,
    next_session_id: usize,
    /// A problem opening a database, shown in the explorer.
    pub(crate) notice: Option<SharedString>,
    pub(crate) store: Option<Arc<Store>>,
    pub(crate) history: Vec<HistoryEntry>,
    pub(crate) history_filter: Entity<InputState>,
    side_panel: SidePanel,
    grid_focus: FocusHandle,
    drag: Option<Drag>,
    /// A failed query's error position, applied to the editor on the next frame.
    pending_error_jump: Option<(usize, usize, usize)>,
    first_frame_traced: bool,
    bench: Option<crate::bench::Bench>,
    bench_pending_sql: Option<String>,
    bench_frame_requested: bool,
    editor_height: f32,
    side_panel_width: f32,
    /// Which text fields have focus, refreshed each render, for focus rings.
    pub(crate) field_focus: FieldFocus,
    /// The launcher's "paste anything" field and what it recognised (text, is_error).
    pub(crate) paste_input: Entity<InputState>,
    pub(crate) paste_note: (SharedString, bool),
    /// Databases found on this Mac, and in a scanned project's .env files.
    pub(crate) found: Vec<Found>,
    pub(crate) project_found: Vec<Found>,
    pub(crate) discovering: bool,
    theme_choice: ThemeChoice,
    pub(crate) palette: Option<(Entity<Palette>, Subscription)>,
    pub(crate) connect_dialog: Option<(Entity<ConnectDialog>, Subscription)>,
    confirm: Option<(Entity<ConfirmDialog>, Subscription, Pending)>,
    pub(crate) saved: Vec<SavedConnection>,
    pub(crate) explorer_filter: Entity<InputState>,
    show_side_panel: bool,
    show_inspector: bool,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(launch: Option<Launch>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let appearance = cx.observe_window_appearance(window, |this: &mut Self, window, cx| {
            if this.theme_choice == ThemeChoice::System {
                theme::apply(Mode::from_appearance(window.appearance()), cx);
                cx.notify();
            }
        });

        let history_filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter history"));
        let explorer_filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter tables"));
        let paste_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(
                "Paste a postgres:// or mysql:// URL, a DATABASE_URL= line, or a database file path",
            )
        });
        let paste_events = cx.subscribe_in(
            &paste_input,
            window,
            |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    this.paste_note = paste_note(&text);
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.connect_pasted(window, cx),
                _ => {}
            },
        );
        let explorer_changes = cx.subscribe(&explorer_filter, |_, _, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                cx.notify();
            }
        });
        let filter_changes = cx.subscribe(&history_filter, |this, _, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.reload_history(cx);
            }
        });

        let store = match Store::default_path().and_then(|path| Store::open(&path)) {
            Ok(store) => Some(Arc::new(store)),
            Err(err) => {
                eprintln!("xenon: query history is unavailable: {err:#}");
                None
            }
        };
        let theme_choice = store
            .as_ref()
            .and_then(|store| store.setting(ThemeChoice::SETTING).ok().flatten())
            .map_or(ThemeChoice::System, |value| ThemeChoice::parse(&value));
        theme::apply(theme_choice.mode(window), cx);

        let mut this = Self {
            focus_handle: cx.focus_handle(),
            tabs: Vec::new(),
            active: 0,
            next_tab_id: 0,
            sessions: Vec::new(),
            next_session_id: 0,
            notice: None,
            store,
            history: Vec::new(),
            history_filter,
            side_panel: SidePanel::Explorer,
            grid_focus: cx.focus_handle(),
            drag: None,
            pending_error_jump: None,
            first_frame_traced: false,
            bench_pending_sql: None,
            bench_frame_requested: false,
            bench: crate::bench::sql().map(|sql| crate::bench::Bench {
                sql,
                phase: crate::bench::Phase::Loading,
            }),
            editor_height: EDITOR_HEIGHT,
            side_panel_width: SIDE_PANEL_WIDTH,
            field_focus: FieldFocus::default(),
            paste_input,
            paste_note: (SharedString::default(), false),
            found: Vec::new(),
            project_found: Vec::new(),
            discovering: false,
            theme_choice,
            palette: None,
            connect_dialog: None,
            confirm: None,
            saved: Vec::new(),
            explorer_filter,
            show_side_panel: true,
            show_inspector: true,
            _subscriptions: vec![appearance, filter_changes, explorer_changes, paste_events],
        };
        this.reload_saved();
        this.rediscover(cx);
        this.add_tab(None, None, window, cx);
        // Deferred so it runs after gpui-component's Root has been built around us.
        cx.defer_in(window, |this: &mut Self, window, cx| {
            this.focus_editor(window, cx)
        });

        match launch {
            Some(Launch::Sqlite(path)) => this.open_path(path, window, cx),
            Some(Launch::Url(engine, url)) => this.connect_url(engine, url, window, cx),
            None => {}
        }
        this
    }

    // ---------------------------------------------------------------- tabs

    #[cfg(test)]
    pub(crate) fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub(crate) fn active_tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    pub(crate) fn session(&self, id: usize) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }

    fn session_mut(&mut self, id: usize) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    /// The connection the active tab runs against.
    pub(crate) fn active_session(&self) -> Option<&Session> {
        self.active_tab().session.and_then(|id| self.session(id))
    }

    fn add_tab(
        &mut self,
        browse: Option<(String, String)>,
        session: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.next_tab_id += 1;
        let id = self.next_tab_id;
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .placeholder("-- write SQL here: ⌘↵ runs the statement, ⇧⌘↵ runs all")
        });
        let (title, browse) = match browse {
            Some((schema, table)) => {
                let filter = cx.new(|cx| {
                    InputState::new(window, cx).placeholder("e.g. status = 'paid' and total > 100")
                });
                let filter_enter = cx.subscribe_in(
                    &filter,
                    window,
                    move |this, _, event: &InputEvent, window, cx| {
                        if let InputEvent::PressEnter { .. } = event
                            && let Some(index) = this.tabs.iter().position(|t| t.id == id)
                        {
                            this.active = index;
                            this.run_browse(window, cx);
                        }
                    },
                );
                let title = SharedString::from(table.clone());
                (
                    title,
                    Some(Browse {
                        schema,
                        table,
                        filter,
                        order: None,
                        _filter_enter: filter_enter,
                    }),
                )
            }
            None if session.is_none() => ("Connect".into(), None),
            None => (
                SharedString::from(format!("Query {}", self.free_query_number())),
                None,
            ),
        };
        self.tabs.push(Tab {
            id,
            session,
            title,
            browse,
            editor,
            query: Query::Idle,
            grid: None,
            selection: None,
            column_widths: HashMap::new(),
            run_origin: None,
        });
        self.active = self.tabs.len() - 1;
        cx.notify();
    }

    /// The lowest n with no open "Query n" tab.
    fn free_query_number(&self) -> usize {
        (1..)
            .find(|n| {
                self.tabs
                    .iter()
                    .all(|tab| tab.title.as_ref() != format!("Query {n}"))
            })
            .expect("unbounded range")
    }

    fn activate_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index < self.tabs.len() {
            let switched = self.tabs[index].session != self.active_tab().session;
            self.active = index;
            if switched {
                self.reload_history(cx);
            }
            self.focus_editor(window, cx);
            cx.notify();
        }
    }

    /// Show the Connect screen in a tab: the open one if there is one, else a new one.
    pub(crate) fn open_connect_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.tabs.iter().position(|tab| tab.session.is_none()) {
            Some(index) => self.active = index,
            None => self.add_tab(None, None, window, cx),
        }
        self.paste_input
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    /// Point a tab at session `id` after connecting: the Connect tab you're on
    /// becomes a query tab for it; otherwise an existing tab for an already
    /// open connection comes forward, or a new query tab opens.
    fn attach(&mut self, id: usize, existing: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_tab().session.is_none() {
            let number = self.free_query_number();
            let tab = &mut self.tabs[self.active];
            tab.session = Some(id);
            tab.title = format!("Query {number}").into();
        } else if let Some(index) = existing
            .then(|| self.tabs.iter().position(|tab| tab.session == Some(id)))
            .flatten()
        {
            self.active = index;
        } else {
            self.add_tab(None, Some(id), window, cx);
        }
        self.reload_history(cx);
        self.focus_editor(window, cx);
        cx.notify();
    }

    fn close_tab_at(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Query::Running { cancel, .. } = &self.tabs[index].query {
            cancel.cancel();
        }
        let session = self.tabs.remove(index).session;
        if self.tabs.is_empty() {
            // There's always one tab; closing the last gives a clean "Query 1".
            self.add_tab(None, session, window, cx);
        } else if self.active >= self.tabs.len() || index < self.active {
            self.active = self.active.saturating_sub(1).min(self.tabs.len() - 1);
        }
        self.focus_editor(window, cx);
        cx.notify();
    }

    fn new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.active_tab().session;
        self.add_tab(None, session, window, cx);
        self.focus_editor(window, cx);
    }

    fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab_at(self.active, window, cx);
    }

    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_tab((self.active + 1) % self.tabs.len(), window, cx);
    }

    fn prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        let len = self.tabs.len();
        self.activate_tab((self.active + len - 1) % len, window, cx);
    }

    fn focus_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editor = self.active_tab().editor.clone();
        editor.update(cx, |editor, cx| editor.focus(window, cx));
    }

    // ---------------------------------------------------------- connecting

    /// Open a SQLite file. Files are remembered with the saved connections.
    pub fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let path_text = path.display().to_string();
        let known = self
            .saved
            .iter()
            .find(|saved| matches!(&saved.target, Target::Sqlite { path } if *path == path_text))
            .cloned();
        let profile = match known {
            Some(saved) => Profile::from_saved(&saved),
            None => Profile {
                saved_id: None,
                save: true,
                name: path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path_text.clone()),
                environment: Environment::None,
                read_only: false,
                target: RequestTarget::Sqlite(path_text),
            },
        };
        self.open_profile(profile, window, cx);
    }

    pub(crate) fn connect_url(
        &mut self,
        engine: ServerEngine,
        url: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match servers::parse_url(engine, &url) {
            Ok(options) => self.open_profile(
                Profile {
                    saved_id: None,
                    save: false,
                    name: servers::default_name(engine, &options),
                    environment: Environment::None,
                    read_only: false,
                    target: RequestTarget::Server(engine, options),
                },
                window,
                cx,
            ),
            Err(err) => {
                self.notice = Some(format!("{err:#}").into());
                cx.notify();
            }
        }
    }

    pub(crate) fn connect_to_saved(
        &mut self,
        saved: &SavedConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_profile(Profile::from_saved(saved), window, cx);
    }

    /// Connect off the main thread (reading a saved password from the Keychain
    /// first), then open it alongside any other connections and remember it if
    /// asked. A connection that's already open is brought forward instead.
    /// Errors go to the connection dialog when it's open.
    fn open_profile(&mut self, profile: Profile, window: &mut Window, cx: &mut Context<Self>) {
        self.notice = None;
        if let Some(id) = profile
            .saved_id
            .and_then(|saved| self.sessions.iter().find(|s| s.saved_id == Some(saved)))
            .map(|s| s.id)
        {
            self.connect_dialog = None;
            self.attach(id, true, window, cx);
            return;
        }
        let store = self.store.clone();
        let task = cx.background_spawn(async move {
            let conn = profile.connect()?;
            let schemas = conn.introspect()?;
            let saved_id = profile.remember(store.as_deref())?;
            anyhow::Ok((conn, schemas, profile, saved_id))
        });
        cx.spawn_in(window, async move |this, cx| {
            let opened = task.await;
            this.update_in(cx, |this, window, cx| {
                match opened {
                    Ok((conn, schemas, profile, saved_id)) => {
                        this.connect_dialog = None;
                        this.reload_saved();
                        // The same database reached another way (a pasted URL for a
                        // saved connection, say): use the one already open.
                        if let Some(id) = this
                            .sessions
                            .iter()
                            .find(|s| s.conn.id() == conn.id())
                            .map(|s| s.id)
                        {
                            this.attach(id, true, window, cx);
                            return;
                        }
                        this.next_session_id += 1;
                        let id = this.next_session_id;
                        this.sessions.push(Session {
                            id,
                            engine: match &profile.target {
                                RequestTarget::Sqlite(_) => Engine::Sqlite,
                                RequestTarget::Server(engine, _) => {
                                    servers::logo(*engine, conn.as_ref())
                                }
                            },
                            version: conn.server_version(),
                            conn,
                            schemas,
                            name: profile.name,
                            environment: profile.environment,
                            read_only: profile.read_only,
                            saved_id,
                            expanded: HashSet::new(),
                            collapsed_schemas: HashSet::new(),
                            collapsed: false,
                        });
                        this.attach(id, false, window, cx);
                        this.bench_start(cx);
                    }
                    Err(err) => {
                        let message: SharedString = format!("{err:#}").into();
                        match &this.connect_dialog {
                            Some((dialog, _)) => {
                                dialog.update(cx, |dialog, cx| dialog.set_error(message, cx))
                            }
                            None => this.notice = Some(message),
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Look for databases on this Mac, off the main thread.
    pub(crate) fn rediscover(&mut self, cx: &mut Context<Self>) {
        if self.discovering {
            return;
        }
        self.discovering = true;
        let task = cx.background_spawn(async move { crate::discover::discover() });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            this.update(cx, |this, cx| {
                this.found = found;
                this.discovering = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Ask for a project folder and list the database URLs in its .env files.
    pub(crate) fn scan_project(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Scan".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            let Some(root) = paths.into_iter().next() else {
                return;
            };
            let found = cx
                .background_spawn(async move { crate::discover::scan_env_files(&root) })
                .await;
            this.update(cx, |this, cx| {
                this.project_found = found;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Connect to whatever is in the paste field.
    fn connect_pasted(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.paste_input.read(cx).value().to_string();
        match detect(&text) {
            Detected::Server(engine, url) => match servers::parse_url(engine, &url) {
                Ok(options) if options.password.is_empty() && !is_local_host(&options.host) => {
                    self.open_connection_dialog(
                        Subject::Prefill {
                            engine,
                            name: String::new(),
                            options,
                        },
                        window,
                        cx,
                    );
                }
                Ok(_) => self.connect_url(engine, url, window, cx),
                Err(err) => self.paste_note = (format!("{err:#}").into(), true),
            },
            Detected::Sqlite(path) => self.open_path(path, window, cx),
            Detected::NotYet(engine) => {
                self.paste_note = (
                    format!("{engine} support is coming soon.").into(),
                    true,
                )
            }
            Detected::Unknown if text.trim().is_empty() => {}
            Detected::Unknown => self.paste_note = (
                "Not recognised. Try a postgres:// or mysql:// URL, a DATABASE_URL= line or a .db file path."
                    .into(),
                true,
            ),
        }
        cx.notify();
    }

    /// Connect to something discovery found, or open the form pre-filled when
    /// it still needs a password.
    pub(crate) fn connect_found(
        &mut self,
        found: &Found,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let FoundTarget::Server(engine, options) = &found.target else {
            return;
        };
        if found.ready() {
            self.open_profile(
                Profile {
                    saved_id: None,
                    save: false,
                    name: found.detail(),
                    environment: Environment::None,
                    read_only: false,
                    target: RequestTarget::Server(*engine, options.clone()),
                },
                window,
                cx,
            );
        } else {
            self.open_connection_dialog(
                Subject::Prefill {
                    engine: *engine,
                    name: found.name.clone(),
                    options: options.clone(),
                },
                window,
                cx,
            );
        }
    }

    /// Disconnect the active tab's connection.
    fn close_connection(
        &mut self,
        _: &CloseConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.active_tab().session {
            self.close_session(id, window, cx);
        }
    }

    /// Disconnect session `id` and close its tabs. With nothing left open, the
    /// window goes back to the Connect screen.
    pub(crate) fn close_session(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.sessions.iter().position(|s| s.id == id) else {
            return;
        };
        self.sessions.remove(index);
        let active_id = self.active_tab().id;
        self.tabs.retain(|tab| {
            let closing = tab.session == Some(id);
            if closing && let Query::Running { cancel, .. } = &tab.query {
                cancel.cancel();
            }
            !closing
        });
        if self.tabs.is_empty() {
            let next = self.sessions.first().map(|s| s.id);
            self.add_tab(None, next, window, cx);
        }
        self.active = self
            .tabs
            .iter()
            .position(|tab| tab.id == active_id)
            .unwrap_or(self.tabs.len() - 1);
        self.reload_history(cx);
        if self.sessions.is_empty() {
            self.rediscover(cx);
            self.paste_input
                .update(cx, |input, cx| input.focus(window, cx));
        } else {
            self.focus_editor(window, cx);
        }
        cx.notify();
    }

    /// Try a connection from the dialog without switching to it.
    fn test_connection(&mut self, request: Request, cx: &mut Context<Self>) {
        let profile = Profile::from_request(request);
        let task = cx.background_spawn(async move {
            let conn = profile.connect()?;
            anyhow::Ok(conn.server_version())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if let Some((dialog, _)) = &this.connect_dialog {
                    dialog.update(cx, |dialog, cx| match result {
                        Ok(version) => {
                            dialog.set_test_passed(format!("Connected · {version}").into(), cx)
                        }
                        Err(err) => dialog.set_error(format!("{err:#}").into(), cx),
                    });
                }
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn reload_saved(&mut self) {
        self.saved = self
            .store
            .as_ref()
            .and_then(|store| store.connections().ok())
            .unwrap_or_default();
    }

    pub(crate) fn new_connection(
        &mut self,
        _: &NewConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_connection_dialog(Subject::New(ServerEngine::Postgres), window, cx);
    }

    /// Connect to a server with settings in hand (no form), without saving it.
    #[cfg(test)]
    pub(crate) fn connect_server(
        &mut self,
        engine: ServerEngine,
        options: ConnectOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = servers::default_name(engine, &options);
        self.open_profile(
            Profile {
                saved_id: None,
                save: false,
                name,
                environment: Environment::None,
                read_only: false,
                target: RequestTarget::Server(engine, options),
            },
            window,
            cx,
        );
    }

    /// The connection form, set up for `engine`.
    pub(crate) fn new_server_connection(
        &mut self,
        engine: ServerEngine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_connection_dialog(Subject::New(engine), window, cx);
    }

    pub(crate) fn edit_saved(
        &mut self,
        saved: &SavedConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let secret = |key: String| secrets::get(&key).ok().flatten().unwrap_or_default();
        let (password, ssh_password) = match &saved.target {
            Target::Server { ssh, .. } => (
                secret(SavedConnection::secret_key(saved.id)),
                if ssh.is_some() {
                    secret(SavedConnection::ssh_secret_key(saved.id))
                } else {
                    String::new()
                },
            ),
            Target::Sqlite { .. } => (String::new(), String::new()),
        };
        self.open_connection_dialog(
            Subject::Saved(saved.clone(), password, ssh_password),
            window,
            cx,
        );
    }

    pub(crate) fn forget_saved(
        &mut self,
        saved: &SavedConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = saved.id;
        let detail = match &saved.target {
            Target::Sqlite { path } => path.clone(),
            Target::Server {
                engine,
                host,
                port,
                database,
                user,
                ..
            } => servers::describe(*engine, host, *port, database, user),
        };
        let body = format!(
            "\"{}\" and its saved password will be removed from Xenon.",
            saved.name
        );
        let dialog = cx.new(|cx| {
            ConfirmDialog::new("Remove saved connection?", body, &detail, None, window, cx)
        });
        self.show_confirm(dialog, Pending::Forget(id), window, cx);
    }

    fn open_connection_dialog(
        &mut self,
        subject: Subject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.connect_dialog.is_some() {
            return;
        }
        let dialog = cx.new(|cx| ConnectDialog::new(subject, window, cx));
        let subscription =
            cx.subscribe_in(&dialog, window, |this, _, event, window, cx| match event {
                ConnectEvent::Submit(request) => {
                    this.open_profile(Profile::from_request(request.clone()), window, cx)
                }
                ConnectEvent::Test(request) => this.test_connection(request.clone(), cx),
                ConnectEvent::Dismiss => {
                    this.connect_dialog = None;
                    this.focus_editor(window, cx);
                    cx.notify();
                }
            });
        self.connect_dialog = Some((dialog, subscription));
        cx.notify();
    }

    fn show_confirm(
        &mut self,
        dialog: Entity<ConfirmDialog>,
        pending: Pending,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let subscription = cx.subscribe_in(&dialog, window, |this, _, event, window, cx| {
            let Some((_, _, pending)) = this.confirm.take() else {
                return;
            };
            if let ConfirmEvent::Confirm = event {
                match pending {
                    Pending::Run {
                        tab_id,
                        sql,
                        origin,
                    } => this.execute(tab_id, sql, origin, cx),
                    Pending::Forget(id) => {
                        if let Some(store) = &this.store
                            && let Err(err) = store.delete_connection(id)
                        {
                            this.notice = Some(format!("{err:#}").into());
                        }
                        for key in [
                            SavedConnection::secret_key(id),
                            SavedConnection::ssh_secret_key(id),
                        ] {
                            if let Err(err) = secrets::delete(&key) {
                                eprintln!("xenon: {err:#}");
                            }
                        }
                        this.reload_saved();
                    }
                }
            }
            this.focus_editor(window, cx);
            cx.notify();
        });
        self.confirm = Some((dialog, subscription, pending));
        cx.notify();
    }

    pub(crate) fn open_file(&mut self, _: &OpenFile, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await
                && let Some(path) = paths.into_iter().next()
            {
                this.update_in(cx, |this, window, cx| this.open_path(path, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    // ------------------------------------------------------------- running

    fn run_statement(&mut self, _: &RunQuery, window: &mut Window, cx: &mut Context<Self>) {
        self.run(RunScope::Statement, window, cx);
    }

    fn run_all(&mut self, _: &RunAll, window: &mut Window, cx: &mut Context<Self>) {
        self.run(RunScope::All, window, cx);
    }

    fn explain(&mut self, _: &Explain, window: &mut Window, cx: &mut Context<Self>) {
        self.run(RunScope::Explain, window, cx);
    }

    pub(crate) fn run(&mut self, scope: RunScope, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.active_session() else {
            self.notice = Some("Open a database first (⌘O or ⌘N).".into());
            cx.notify();
            return;
        };
        let tab = &self.tabs[self.active];
        if matches!(tab.query, Query::Running { .. }) {
            return;
        }

        let editor = tab.editor.read(cx);
        let text = editor.value().to_string();
        let selection = editor.selected_range();
        let range = if !selection.is_empty() && text.get(selection.clone()).is_some() {
            selection
        } else {
            match scope {
                RunScope::All => 0..text.len(),
                RunScope::Statement | RunScope::Explain => {
                    match statement_at(&text, editor.cursor()) {
                        Some(range) => range,
                        None => return,
                    }
                }
            }
        };
        let origin = (scope != RunScope::Explain).then_some(range.start);
        let sql = text[range].to_string();
        if sql.trim().is_empty() {
            return;
        }
        let sql = match scope {
            RunScope::Explain => session.conn.explain(sql.trim().trim_end_matches(';')),
            _ => sql,
        };
        let tab_id = tab.id;

        // Guard rails: risky statements need a confirmation, and on production
        // every write needs the connection's name typed. Read-only connections
        // are already protected by the server.
        if !session.read_only
            && let Some((risk, what)) = assess(&sql)
            && (session.environment == Environment::Production || risk >= Risk::EveryRow)
        {
            let production = session.environment == Environment::Production;
            let body = match risk {
                Risk::Destructive => format!("This runs {what}, which can't be undone."),
                Risk::EveryRow => format!("This runs {what}, so it changes every row."),
                Risk::Write => format!("This {what} statement writes to the database."),
            };
            let body = if production {
                format!("{body} {} is a production connection.", session.name)
            } else {
                body
            };
            let title = if production {
                "Run on production?"
            } else {
                "Run this statement?"
            };
            let require = production.then(|| session.name.clone());
            let dialog = cx.new(|cx| ConfirmDialog::new(title, body, &sql, require, window, cx));
            self.show_confirm(
                dialog,
                Pending::Run {
                    tab_id,
                    sql,
                    origin,
                },
                window,
                cx,
            );
            return;
        }
        self.execute(tab_id, sql, origin, cx);
    }

    /// Run `sql` for a tab in the background, recording it in the history.
    fn execute(
        &mut self,
        tab_id: usize,
        sql: String,
        origin: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let Some(session_id) = self
            .tabs
            .iter()
            .find(|t| t.id == tab_id)
            .and_then(|t| t.session)
        else {
            return;
        };
        let Some(session) = self.session(session_id) else {
            return;
        };
        let conn = session.conn.clone();
        let store = self.store.clone();
        let cancel = conn.cancel_handle();
        let work = cx.background_spawn(async move {
            let started = std::time::Instant::now();
            let result = conn.execute(&sql, ROW_LIMIT);
            crate::trace("execute query", started);
            if let Some(store) = store {
                let (rows, error) = match &result {
                    Ok(result) => (
                        Some(match &result.outcome {
                            QueryOutcome::Rows(rows) => rows.rows.len() as u64,
                            QueryOutcome::Affected(n) => *n,
                        }),
                        None,
                    ),
                    Err(err) => (None, Some(format!("{err:#}"))),
                };
                let recorded = store.record(NewHistoryEntry {
                    connection: &conn.id(),
                    sql: &sql,
                    duration: started.elapsed(),
                    rows,
                    error: error.as_deref(),
                });
                if let Err(err) = recorded {
                    eprintln!("xenon: could not save query history: {err:#}");
                }
            }
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
            this.update(cx, |this, cx| {
                this.finish_query(tab_id, session_id, result, schemas, cx)
            })
            .ok();
        });
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) else {
            return;
        };
        tab.run_origin = origin;
        tab.query = Query::Running {
            cancel,
            _task: task,
        };
        cx.notify();
    }

    fn finish_query(
        &mut self,
        tab_id: usize,
        session_id: usize,
        result: anyhow::Result<QueryResult>,
        schemas: Option<Vec<Schema>>,
        cx: &mut Context<Self>,
    ) {
        if let (Some(session), Some(schemas)) = (self.session_mut(session_id), schemas) {
            session.schemas = schemas;
        }
        self.reload_history(cx);
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) else {
            return;
        };
        tab.selection = None;
        match result {
            Ok(mut result) => {
                if let QueryOutcome::Rows(rows) = &mut result.outcome {
                    let started = std::time::Instant::now();
                    tab.grid = Some(Grid::new(std::mem::take(rows)));
                    crate::trace("build results grid", started);
                    result.outcome = QueryOutcome::Rows(Default::default());
                } else {
                    tab.grid = None;
                }
                tab.query = Query::Done(Arc::new(result));
                if let Some(bench) = &mut self.bench
                    && matches!(bench.phase, crate::bench::Phase::Loading)
                {
                    bench.phase = crate::bench::Phase::Idle(crate::bench::Mark::now());
                    self.bench_schedule(cx);
                }
            }
            Err(err) => {
                tab.grid = None;
                tab.query = Query::Failed(format!("{err:#}").into());
                let position = err.downcast_ref::<QueryError>().and_then(|e| e.position);
                if let (Some(position), Some(origin)) = (position, tab.run_origin) {
                    self.pending_error_jump = Some((tab_id, origin, position));
                }
            }
        }
        cx.notify();
    }

    // ------------------------------------------------------------- bench

    fn bench_start(&mut self, cx: &mut Context<Self>) {
        let Some(bench) = &self.bench else { return };
        let sql = bench.sql.clone();
        // Setting the editor's text needs a window; render does it.
        self.bench_pending_sql = Some(sql.clone());
        self.execute(self.active_tab().id, sql, None, cx);
    }

    fn bench_schedule(&mut self, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(crate::bench::PHASE).await;
            this.update(cx, |this, cx| {
                if let Some(bench) = &mut this.bench
                    && let crate::bench::Phase::Idle(mark) = bench.phase
                {
                    bench.phase = crate::bench::Phase::Scrolling {
                        mark: crate::bench::Mark::now(),
                        frames: 0,
                        row: 0,
                        idle_cpu: mark.cpu_percent(),
                    };
                    this.bench_frame_requested = true;
                    this.notify_grid(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// One frame of the scrolling phase: move the grid and ask for another frame.
    fn bench_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(bench) = &mut self.bench else { return };
        let crate::bench::Phase::Scrolling {
            mark,
            frames,
            row,
            idle_cpu,
        } = &mut bench.phase
        else {
            return;
        };
        *frames += 1;
        *row += crate::bench::ROWS_PER_FRAME;
        if mark.elapsed() >= crate::bench::PHASE {
            let seconds = mark.elapsed().as_secs_f64();
            eprintln!(
                "xenon bench: idle {:.1}% cpu | scrolling {:.1}% cpu, {:.0} fps",
                idle_cpu,
                mark.cpu_percent(),
                *frames as f64 / seconds
            );
            bench.phase = crate::bench::Phase::Done;
            cx.quit();
            return;
        }
        let row = *row;
        if let Some(grid) = &self.active_tab().grid
            && grid.row_count() > 0
        {
            grid.scroll
                .scroll_to_item(row % grid.row_count(), ScrollStrategy::Top);
        }
        self.notify_grid(cx);
        let this = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            this.update(cx, |this, cx| this.bench_tick(window, cx)).ok();
        });
    }

    /// Ask for the results grid to be drawn again.
    fn notify_grid(&mut self, cx: &mut Context<Self>) {
        cx.notify();
    }

    /// Put the editor's cursor on the word a failed statement points at.
    fn jump_to_error(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((tab_id, origin, position)) = self.pending_error_jump.take() else {
            return;
        };
        if self.active_tab().id != tab_id {
            return;
        }
        let editor = self.active_tab().editor.clone();
        editor.update(cx, |editor, cx| {
            let text = editor.value().to_string();
            let Some(tail) = text.get(origin..) else {
                return;
            };
            // Positions count characters from 1.
            let Some((offset, _)) = tail.char_indices().nth(position.saturating_sub(1)) else {
                return;
            };
            let start = origin + offset;
            let end = start
                + text[start..]
                    .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .unwrap_or(text.len() - start);
            editor.set_selected_range(start..end.max(start), cx);
            editor.focus(window, cx);
        });
    }

    fn cancel_query(&mut self, _: &CancelQuery, _: &mut Window, _: &mut Context<Self>) {
        if let Query::Running { cancel, .. } = &self.active_tab().query {
            cancel.cancel();
        }
    }

    /// Show a table's rows, in the tab already browsing it or in a new one.
    pub(crate) fn browse(
        &mut self,
        session: usize,
        schema: &str,
        table: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.session(session).is_none() {
            return;
        }
        let key = format!("{schema}.{table}");
        match self.tabs.iter().position(|tab| {
            tab.session == Some(session) && tab.browse.as_ref().is_some_and(|b| b.key() == key)
        }) {
            Some(index) => self.activate_tab(index, window, cx),
            None => self.add_tab(
                Some((schema.to_string(), table.to_string())),
                Some(session),
                window,
                cx,
            ),
        }
        self.reload_history(cx);
        self.run_browse(window, cx);
    }

    /// Regenerate the active browse tab's query from its filter and sort, and run it.
    fn run_browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(session), Some(browse)) = (self.active_session(), &self.active_tab().browse)
        else {
            return;
        };
        let conn = session.conn.clone();
        // Leave out the schema names resolve to anyway: `from orders`, not `from "public"."orders"`.
        let schema = (conn.default_schema().as_deref() != Some(browse.schema.as_str()))
            .then_some(browse.schema.as_str());
        let sql = browse_table(
            |ident| conn.ident(ident),
            schema,
            &browse.table,
            &browse.filter.read(cx).value(),
            browse
                .order
                .as_ref()
                .map(|(column, dir)| (column.as_str(), *dir)),
            BROWSE_LIMIT,
        );
        let editor = self.active_tab().editor.clone();
        editor.update(cx, |editor, cx| editor.set_value(sql, window, cx));
        self.run(RunScope::All, window, cx);
    }

    fn clear_browse_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(browse) = &mut self.tabs[self.active].browse {
            browse.order = None;
            let filter = browse.filter.clone();
            filter.update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.run_browse(window, cx);
    }

    // ---------------------------------------------------------------- grid

    /// Header click: browse tabs sort on the server (the whole table, not just
    /// the rows fetched); query tabs sort the rows they already have.
    fn sort_column(&mut self, column: usize, window: &mut Window, cx: &mut Context<Self>) {
        let tab = &mut self.tabs[self.active];
        let Some(grid) = &mut tab.grid else { return };
        let name = grid.data.columns[column].name.clone();
        tab.selection = None;
        if let Some(browse) = &mut tab.browse {
            let current = browse
                .order
                .as_ref()
                .filter(|(c, _)| *c == name)
                .map(|(_, d)| *d);
            browse.order = SortDirection::cycle(current).map(|dir| (name, dir));
            self.run_browse(window, cx);
        } else {
            let current = grid.sort.filter(|(c, _)| *c == column).map(|(_, d)| d);
            grid.set_sort(SortDirection::cycle(current).map(|dir| (column, dir)));
            cx.notify();
        }
    }

    pub(crate) fn select_cell(
        &mut self,
        row: usize,
        column: usize,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = &mut self.tabs[self.active];
        tab.selection = Some(match (tab.selection, extend) {
            (Some(current), true) => Selection {
                head: (row, column),
                ..current
            },
            _ => Selection::cell(row, column),
        });
        self.grid_focus.focus(window, cx);
        cx.notify();
    }

    fn step_selection(&mut self, dr: isize, dc: isize, extend: bool, cx: &mut Context<Self>) {
        let tab = &mut self.tabs[self.active];
        let Some(grid) = &tab.grid else { return };
        let (rows, columns) = (grid.row_count(), grid.data.columns.len());
        if rows == 0 || columns == 0 {
            return;
        }
        let next = match tab.selection {
            Some(current) => current.step(dr, dc, extend, rows, columns),
            None => Selection::cell(0, 0),
        };
        grid.scroll
            .scroll_to_item(next.head.0, ScrollStrategy::Nearest);
        tab.selection = Some(next);
        cx.notify();
    }

    fn select_all_cells(&mut self, _: &SelectAllCells, _: &mut Window, cx: &mut Context<Self>) {
        let tab = &mut self.tabs[self.active];
        if let Some(grid) = &tab.grid
            && grid.row_count() > 0
            && !grid.data.columns.is_empty()
        {
            tab.selection = Some(Selection {
                anchor: (0, 0),
                head: (grid.row_count() - 1, grid.data.columns.len() - 1),
            });
            cx.notify();
        }
    }

    fn copy(&mut self, format: Format, cx: &mut Context<Self>) {
        let tab = self.active_tab();
        let (Some(grid), Some(selection)) = (&tab.grid, tab.selection) else {
            return;
        };
        let columns: Vec<_> = selection.columns().map(|c| &grid.data.columns[c]).collect();
        let rows: Vec<Vec<_>> = selection
            .rows()
            .map(|r| selection.columns().map(|c| &grid.row(r)[c]).collect())
            .collect();
        cx.write_to_clipboard(ClipboardItem::new_string(export::render(
            format, &columns, &rows,
        )));
    }

    fn drag_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = &self.drag else { return };
        if event.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        match drag {
            Drag::Column {
                tab_id,
                column,
                start_x,
                start_width,
            } => {
                let width =
                    (start_width + f32::from(event.position.x - *start_x)).max(results::MIN_COLUMN);
                let (tab_id, column) = (*tab_id, column.clone());
                if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                    tab.column_widths.insert(column, width.round());
                }
            }
            Drag::SidePanel {
                start_x,
                start_width,
            } => {
                self.side_panel_width = (start_width + f32::from(event.position.x - *start_x))
                    .clamp(MIN_SIDE_PANEL_WIDTH, MAX_SIDE_PANEL_WIDTH)
                    .round();
            }
            Drag::Split {
                start_y,
                start_height,
            } => {
                let max = f32::from(window.viewport_size().height) - MIN_RESULTS_HEIGHT;
                self.editor_height = (start_height + f32::from(event.position.y - *start_y))
                    .clamp(MIN_EDITOR_HEIGHT, max.max(MIN_EDITOR_HEIGHT))
                    .round();
            }
        }
        cx.notify();
    }

    // ------------------------------------------------------------- palette

    pub(crate) fn open_palette(
        &mut self,
        _: &OpenPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.palette.is_some() {
            return;
        }
        let mut items = Vec::new();
        // Tables from every open connection, the active one's first.
        let active = self.active_tab().session;
        let mut sessions: Vec<&Session> = self.sessions.iter().collect();
        sessions.sort_by_key(|s| Some(s.id) != active);
        let several = sessions.len() > 1;
        for session in sessions {
            let nested = session.shows_schemas();
            for schema in &session.schemas {
                for object in &schema.objects {
                    let detail = match (nested, several) {
                        (true, true) => Some(format!("{} · {}", schema.name, session.name)),
                        (true, false) => Some(schema.name.clone()),
                        (false, true) => Some(session.name.clone()),
                        (false, false) => None,
                    };
                    items.push(palette::Item {
                        kind: match object.kind {
                            ObjectKind::Table => "TABLE",
                            ObjectKind::View => "VIEW",
                        },
                        label: object.name.clone().into(),
                        detail: detail.map(Into::into),
                        keys: None,
                        target: palette::Target::Table {
                            session: session.id,
                            schema: schema.name.clone(),
                            table: object.name.clone(),
                        },
                    });
                }
            }
        }
        // Keys are listed here rather than looked up: several bindings only
        // apply inside the editor or grid, which the palette's focus isn't.
        let commands: [(&str, &str, Box<dyn gpui::Action>); 23] = [
            ("Run Statement", "⌘↵", Box::new(RunQuery)),
            ("Run All", "⇧⌘↵", Box::new(RunAll)),
            ("Explain", "⌘E", Box::new(Explain)),
            ("Cancel Query", "⌘.", Box::new(CancelQuery)),
            ("New Query Tab", "⌘T", Box::new(NewTab)),
            ("Close Tab", "⌘W", Box::new(CloseTab)),
            ("Next Tab", "⌃⇥", Box::new(NextTab)),
            ("Previous Tab", "⌃⇧⇥", Box::new(PrevTab)),
            ("Connections…", "⇧⌘N", Box::new(ShowConnections)),
            ("Disconnect", "⇧⌘W", Box::new(CloseConnection)),
            ("Open SQLite File…", "⌘O", Box::new(OpenFile)),
            ("New Connection…", "⌘N", Box::new(NewConnection)),
            ("Show Explorer", "⇧⌘E", Box::new(ShowExplorer)),
            ("Show History", "⌘Y", Box::new(ShowHistory)),
            ("Toggle Side Panel", "⌘B", Box::new(ToggleExplorer)),
            ("Toggle Inspector", "⌘I", Box::new(ToggleInspector)),
            ("Copy Cells as CSV", "⇧⌘C", Box::new(CopyCsv)),
            ("Copy Cells as JSON", "⌥⌘C", Box::new(CopyJson)),
            ("Theme: Match System", "", Box::new(UseSystemTheme)),
            ("Theme: Light", "", Box::new(UseLightTheme)),
            ("Theme: Dark", "", Box::new(UseDarkTheme)),
            ("Focus Editor", "", Box::new(FocusEditor)),
            ("Quit Xenon", "⌘Q", Box::new(Quit)),
        ];
        for (label, keys, action) in commands {
            items.push(palette::Item {
                kind: "COMMAND",
                label: label.into(),
                detail: None,
                keys: (!keys.is_empty()).then(|| keys.into()),
                target: palette::Target::Action(action),
            });
        }

        let palette = cx.new(|cx| Palette::new(items, window, cx));
        let subscription = cx.subscribe_in(&palette, window, |this, _, event, window, cx| {
            this.palette = None;
            this.focus_editor(window, cx);
            match event {
                PaletteEvent::Pick(palette::Target::Table {
                    session,
                    schema,
                    table,
                }) => {
                    let (session, schema, table) = (*session, schema.clone(), table.clone());
                    this.browse(session, &schema, &table, window, cx);
                }
                PaletteEvent::Pick(palette::Target::Action(action)) => {
                    window.dispatch_action(action.boxed_clone(), cx);
                }
                PaletteEvent::Dismiss => {}
            }
            cx.notify();
        });
        self.palette = Some((palette, subscription));
        cx.notify();
    }

    fn set_theme(&mut self, choice: ThemeChoice, window: &mut Window, cx: &mut Context<Self>) {
        self.theme_choice = choice;
        theme::apply(choice.mode(window), cx);
        if let Some(store) = &self.store
            && let Err(err) = store.set_setting(ThemeChoice::SETTING, choice.as_str())
        {
            eprintln!("xenon: could not save the theme: {err:#}");
        }
        cx.notify();
    }

    // -------------------------------------------------------------- panels

    pub(crate) fn toggle_expanded(&mut self, session: usize, key: &str, cx: &mut Context<Self>) {
        if let Some(session) = self.session_mut(session)
            && !session.expanded.remove(key)
        {
            session.expanded.insert(key.to_string());
        }
        cx.notify();
    }

    pub(crate) fn toggle_schema(&mut self, session: usize, schema: &str, cx: &mut Context<Self>) {
        if let Some(session) = self.session_mut(session)
            && !session.collapsed_schemas.remove(schema)
        {
            session.collapsed_schemas.insert(schema.to_string());
        }
        cx.notify();
    }

    pub(crate) fn toggle_session(&mut self, session: usize, cx: &mut Context<Self>) {
        if let Some(session) = self.session_mut(session) {
            session.collapsed = !session.collapsed;
        }
        cx.notify();
    }

    fn show_panel(&mut self, panel: SidePanel, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_side_panel && self.side_panel == panel {
            self.show_side_panel = false;
        } else {
            self.show_side_panel = true;
            self.side_panel = panel;
            if panel == SidePanel::History {
                self.reload_history(cx);
                self.history_filter
                    .update(cx, |input, cx| input.focus(window, cx));
            }
        }
        cx.notify();
    }

    fn show_explorer(&mut self, _: &ShowExplorer, window: &mut Window, cx: &mut Context<Self>) {
        self.show_panel(SidePanel::Explorer, window, cx);
    }

    pub(crate) fn show_history(
        &mut self,
        _: &ShowHistory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_panel(SidePanel::History, window, cx);
    }

    fn toggle_explorer(&mut self, _: &ToggleExplorer, _: &mut Window, cx: &mut Context<Self>) {
        self.show_side_panel = !self.show_side_panel;
        cx.notify();
    }

    fn toggle_inspector(&mut self, _: &ToggleInspector, _: &mut Window, cx: &mut Context<Self>) {
        self.show_inspector = !self.show_inspector;
        cx.notify();
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.first_frame_traced {
            self.first_frame_traced = true;
            if let Some(launched) = crate::LAUNCHED.get() {
                crate::trace("first frame", *launched);
            }
        }
        if self.pending_error_jump.is_some() {
            self.jump_to_error(window, cx);
        }
        if let Some(sql) = self.bench_pending_sql.take() {
            let editor = self.active_tab().editor.clone();
            editor.update(cx, |editor, cx| editor.set_value(sql, window, cx));
        }
        if std::mem::take(&mut self.bench_frame_requested) {
            let this = cx.entity().downgrade();
            window.on_next_frame(move |window, cx| {
                this.update(cx, |this, cx| this.bench_tick(window, cx)).ok();
            });
        }
        let focused = |input: &Entity<InputState>, cx: &App| {
            gpui::Focusable::focus_handle(input.read(cx), cx).is_focused(window)
        };
        self.field_focus = FieldFocus {
            paste: focused(&self.paste_input, cx),
            explorer: focused(&self.explorer_filter, cx),
            history: focused(&self.history_filter, cx),
            browse: self
                .active_tab()
                .browse
                .as_ref()
                .is_some_and(|b| focused(&b.filter, cx)),
        };
        let theme = cx.theme().clone();

        let body = div()
            .flex()
            .flex_row()
            .flex_1()
            .min_h(px(0.))
            .child(self.render_rail(&theme, cx))
            .when(self.show_side_panel, |el| {
                el.child(self.render_side_panel(&theme, cx))
            })
            .child(self.render_center(&theme, cx))
            .when(
                self.show_inspector && self.active_session().is_some(),
                |el| el.child(self.render_inspector(&theme)),
            );

        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::open_file))
            .on_action(cx.listener(Self::new_connection))
            .on_action(cx.listener(Self::run_statement))
            .on_action(cx.listener(Self::run_all))
            .on_action(cx.listener(Self::explain))
            .on_action(cx.listener(Self::select_all_cells))
            .on_action(cx.listener(|this, _: &MoveUp, _, cx| this.step_selection(-1, 0, false, cx)))
            .on_action(
                cx.listener(|this, _: &MoveDown, _, cx| this.step_selection(1, 0, false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &MoveLeft, _, cx| this.step_selection(0, -1, false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &MoveRight, _, cx| this.step_selection(0, 1, false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ExtendUp, _, cx| this.step_selection(-1, 0, true, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ExtendDown, _, cx| this.step_selection(1, 0, true, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ExtendLeft, _, cx| this.step_selection(0, -1, true, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ExtendRight, _, cx| this.step_selection(0, 1, true, cx)),
            )
            .on_action(cx.listener(|this, _: &CopySelection, _, cx| this.copy(Format::Tsv, cx)))
            .on_action(cx.listener(|this, _: &CopyCsv, _, cx| this.copy(Format::Csv, cx)))
            .on_action(cx.listener(|this, _: &CopyJson, _, cx| this.copy(Format::Json, cx)))
            .on_action(
                cx.listener(|this, _: &FocusEditor, window, cx| this.focus_editor(window, cx)),
            )
            .on_action(cx.listener(Self::open_palette))
            .on_action(cx.listener(Self::close_connection))
            .on_action(cx.listener(|this, _: &ShowConnections, window, cx| {
                this.open_connect_tab(window, cx)
            }))
            .on_action(cx.listener(|this, _: &UseSystemTheme, w, cx| {
                this.set_theme(ThemeChoice::System, w, cx)
            }))
            .on_action(cx.listener(|this, _: &UseLightTheme, w, cx| {
                this.set_theme(ThemeChoice::Light, w, cx)
            }))
            .on_action(
                cx.listener(|this, _: &UseDarkTheme, w, cx| {
                    this.set_theme(ThemeChoice::Dark, w, cx)
                }),
            )
            .on_mouse_move(cx.listener(Self::drag_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.drag = None),
            )
            .on_action(cx.listener(Self::cancel_query))
            .on_action(cx.listener(Self::new_tab))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::prev_tab))
            .on_action(cx.listener(Self::show_explorer))
            .on_action(cx.listener(Self::show_history))
            .on_action(cx.listener(Self::toggle_explorer))
            .on_action(cx.listener(Self::toggle_inspector))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                if let Some(path) = paths.0.first() {
                    this.open_path(path.clone(), window, cx);
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.s1)
            .font_family(UI_FONT)
            .text_color(theme.text)
            .text_size(px(12.))
            .relative()
            .child(self.render_titlebar(&theme, cx))
            .child(body)
            .child(self.render_statusbar(&theme))
            .when_some(self.connect_dialog.as_ref(), |el, (dialog, _)| {
                el.child(dialog.clone())
            })
            .when_some(self.palette.as_ref(), |el, (palette, _)| {
                el.child(palette.clone())
            })
            .when_some(self.confirm.as_ref(), |el, (dialog, _, _)| {
                el.child(dialog.clone())
            })
    }
}

impl Workspace {
    fn render_titlebar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let several = self.sessions.len() > 1;
        let tabs = self.tabs.iter().enumerate().map(|(index, tab)| {
            let active = index == self.active;
            let running = matches!(tab.query, Query::Running { .. });
            let session = tab.session.and_then(|id| self.session(id));
            // Production tabs are underlined in red instead of the accent.
            let underline = match session {
                Some(session) if session.environment == Environment::Production => theme.err,
                _ => theme.acc,
            };
            let mark = match session {
                Some(session) => widgets::engine_logo(
                    session.engine,
                    12.,
                    if active { theme.acc } else { theme.t3 },
                ),
                None => svg()
                    .path(assets::icon("plus"))
                    .size(px(12.))
                    .flex_none()
                    .text_color(if active { theme.acc } else { theme.t3 }),
            };
            div()
                .id(("tab", tab.id))
                .group("tab")
                .h_full()
                .max_w(px(TAB_MAX_WIDTH))
                .pl(px(14.))
                .pr(px(8.))
                .flex()
                .items_center()
                .gap(px(8.))
                .relative()
                .border_r_1()
                .border_color(theme.line)
                .cursor_pointer()
                .when(active, |el| el.bg(theme.s1).text_color(theme.text))
                .when(!active, |el| {
                    el.text_color(theme.t3).hover(|s| s.text_color(theme.t2))
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.activate_tab(index, window, cx);
                }))
                .child(mark)
                .child(
                    div()
                        .flex()
                        .min_w(px(0.))
                        .gap(px(5.))
                        .child(div().flex_none().child(tab.title.clone()))
                        // Which connection, once there's more than one to tell apart.
                        .when_some(session.filter(|_| several), |el, session| {
                            el.child(
                                div()
                                    .truncate()
                                    .text_color(theme.t3)
                                    .child(session.name.clone()),
                            )
                        }),
                )
                .when(running, |el| {
                    el.child(div().size(px(6.)).flex_none().rounded_full().bg(theme.acc))
                })
                .child(
                    div()
                        .id(("close-tab", tab.id))
                        .size(px(18.))
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        // Hidden on inactive tabs until the tab is hovered.
                        .when(!active, |el| {
                            el.opacity(0.).group_hover("tab", |s| s.opacity(1.))
                        })
                        .hover(|s| s.bg(theme.s3))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_tab_at(index, window, cx);
                        }))
                        .child(
                            svg()
                                .path(assets::icon("x"))
                                .size(px(12.))
                                .text_color(theme.t2),
                        ),
                )
                .when(active, |el| {
                    el.child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom_0()
                            .h(px(2.))
                            .bg(underline),
                    )
                })
        });

        div()
            .id("titlebar")
            .h(px(TITLEBAR_HEIGHT))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .bg(theme.bg)
            .border_b_1()
            .border_color(theme.line)
            // Double-clicking empty title bar space zooms the window, like any Mac app.
            .on_click(|event: &ClickEvent, window, _| {
                if event.click_count() == 2 {
                    window.titlebar_double_click();
                }
            })
            .child(div().w(px(TRAFFIC_LIGHT_INSET)).flex_none().h_full())
            .child(
                div()
                    .id("toggle-side-panel")
                    .h_full()
                    .px(px(10.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .border_r_1()
                    .border_color(theme.line)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.s3))
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.toggle_explorer(&ToggleExplorer, window, cx);
                    }))
                    .child(
                        svg()
                            .path(assets::icon(if self.show_side_panel {
                                "panel-left-close"
                            } else {
                                "panel-left-open"
                            }))
                            .size(px(16.))
                            .text_color(theme.t2),
                    ),
            )
            .when(!self.sessions.is_empty(), |el| {
                el.child(
                    div()
                        .id("tab-strip")
                        .h_full()
                        .flex()
                        .flex_row()
                        .min_w(px(0.))
                        .overflow_x_scroll()
                        .children(tabs),
                )
                .child(
                    div()
                        .id("new-tab")
                        .ml(px(6.))
                        .size(px(24.))
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .rounded(px(RADIUS_CONTROL))
                        .text_color(theme.t3)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.s3).text_color(theme.text))
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.new_tab(&NewTab, window, cx);
                        }))
                        .child(
                            svg()
                                .path(assets::icon("plus"))
                                .size(px(14.))
                                .text_color(theme.t3),
                        ),
                )
            })
            .child(div().flex_1().h_full())
            .child(
                div()
                    .id("go-to-anything")
                    .cursor_pointer()
                    .hover(|s| s.border_color(theme.t3))
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_palette(&OpenPalette, window, cx);
                    }))
                    .mr(px(8.))
                    .w(px(260.))
                    .h(px(26.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(7.))
                    .pl(px(9.))
                    .pr(px(4.))
                    .rounded(px(6.))
                    .bg(theme.s2)
                    .border_1()
                    .border_color(theme.line)
                    .text_color(theme.t3)
                    .text_size(px(12.))
                    .child(
                        svg()
                            .path(assets::icon("search"))
                            .size(px(13.))
                            .flex_none()
                            .text_color(theme.t3),
                    )
                    .child(div().flex_1().child("Go to anything"))
                    .child(kbd(theme, "⌘K")),
            )
    }

    fn render_rail(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let item = |id: &'static str, icon: &'static str, panel: SidePanel| {
            let active = self.show_side_panel && self.side_panel == panel;
            div()
                .id(id)
                .size(px(36.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .rounded(px(6.))
                .when(active, |el| el.bg(theme.acc_wash))
                .when(!active, |el| el.hover(|s| s.bg(theme.s3)))
                .on_click(
                    cx.listener(move |this, _, window, cx| this.show_panel(panel, window, cx)),
                )
                .child(
                    svg()
                        .path(assets::icon(icon))
                        .size(px(18.))
                        .text_color(if active { theme.acc } else { theme.t2 }),
                )
        };

        div()
            .w(px(RAIL_WIDTH))
            .flex_none()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(8.))
            .gap(px(4.))
            .bg(theme.bg)
            .border_r_1()
            .border_color(theme.line)
            .child(item("rail-explorer", "database", SidePanel::Explorer))
            .child(item("rail-history", "clock", SidePanel::History))
    }

    fn render_side_panel(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let (label, content) = match self.side_panel {
            SidePanel::Explorer => ("Explorer", self.render_explorer(theme, cx)),
            SidePanel::History => ("History", self.render_history(theme, cx)),
        };
        div()
            .w(px(self.side_panel_width))
            .flex_none()
            .relative()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme.line)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .pr(px(8.))
                    .child(panel_label(theme, label))
                    .when(self.side_panel == SidePanel::Explorer, |el| {
                        el.child(
                            div()
                                .id("explorer-connect")
                                .mt(px(4.))
                                .size(px(22.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(RADIUS_CONTROL))
                                .cursor_pointer()
                                .hover(|s| s.bg(theme.s3))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_connect_tab(window, cx)
                                }))
                                .child(
                                    svg()
                                        .path(assets::icon("plus"))
                                        .size(px(14.))
                                        .text_color(theme.t2),
                                ),
                        )
                    }),
            )
            .child(content)
            .child(
                // Drag the edge to resize; double-click to reset.
                div()
                    .id("side-panel-edge")
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(px(-3.))
                    .w(px(6.))
                    .cursor_col_resize()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                            if event.click_count >= 2 {
                                this.side_panel_width = SIDE_PANEL_WIDTH;
                                cx.notify();
                            } else {
                                this.drag = Some(Drag::SidePanel {
                                    start_x: event.position.x,
                                    start_width: this.side_panel_width,
                                });
                            }
                        }),
                    ),
            )
    }

    fn render_center(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        if self.active_session().is_none() {
            return div()
                .flex_1()
                .min_w(px(0.))
                .child(self.render_launcher(theme, cx))
                .into_any_element();
        }
        let tab = self.active_tab();
        let editor = div()
            .h(px(self.editor_height))
            .flex_none()
            .flex()
            .flex_col()
            .bg(theme.s2)
            .border_b_1()
            .border_color(theme.line)
            .child(self.render_toolbar(theme, cx))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .pt(px(6.))
                    .font_family(MONO_FONT)
                    .text_size(px(12.5))
                    .child(Editor::new(&tab.editor).h_full().appearance(false)),
            );

        div()
            .flex_1()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .child(editor)
            .child(
                div()
                    .id("split")
                    .h(px(5.))
                    .mt(px(-3.))
                    .mb(px(-2.))
                    .flex_none()
                    .cursor_row_resize()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                            if event.click_count >= 2 {
                                this.editor_height = EDITOR_HEIGHT;
                                cx.notify();
                            } else {
                                this.drag = Some(Drag::Split {
                                    start_y: event.position.y,
                                    start_height: this.editor_height,
                                });
                            }
                        }),
                    ),
            )
            .when_some(tab.browse.as_ref(), |el, browse| {
                el.child(self.render_filter_bar(browse, theme, cx))
            })
            .child(self.render_results_bar(theme))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .child(self.render_results(theme, cx)),
            )
            .into_any_element()
    }

    fn render_toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = self.active_tab();
        let running = matches!(tab.query, Query::Running { .. });
        let small = |button: gpui::Stateful<gpui::Div>| button.h(px(26.)).px(px(10.));

        // Where this query runs: the connection, then what the tab is.
        let context = self.active_session().map(|session| {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .min_w(px(0.))
                .text_color(theme.t3)
                .child(
                    svg()
                        .path(assets::icon("database"))
                        .size(px(13.))
                        .flex_none()
                        .text_color(if session.environment == Environment::Production {
                            theme.err
                        } else {
                            theme.t3
                        }),
                )
                .child(
                    div()
                        .text_color(theme.t2)
                        .truncate()
                        .child(session.name.clone()),
                )
                .child("/")
                .child(div().truncate().child(tab.title.clone()))
        });

        let mut bar = div()
            .h(px(40.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.))
            .pl(px(14.))
            .pr(px(8.))
            .border_b_1()
            .border_color(theme.line_2)
            .font_family(UI_FONT)
            .children(context)
            .child(div().flex_1());
        if running {
            bar = bar.child(
                small(widgets::button(
                    theme,
                    "cancel",
                    "Cancel",
                    ButtonKind::Secondary,
                    true,
                ))
                .child(kbd(theme, "⌘."))
                .on_click(cx.listener(|this, _, w, cx| this.cancel_query(&CancelQuery, w, cx))),
            );
        } else {
            bar = bar
                .child(
                    small(widgets::button(
                        theme,
                        "explain",
                        "Explain",
                        ButtonKind::Ghost,
                        true,
                    ))
                    .on_click(cx.listener(|this, _, w, cx| this.run(RunScope::Explain, w, cx))),
                )
                .child(
                    small(widgets::button(
                        theme,
                        "run-all",
                        "Run all",
                        ButtonKind::Ghost,
                        true,
                    ))
                    .on_click(cx.listener(|this, _, w, cx| this.run(RunScope::All, w, cx))),
                )
                .child(
                    small(widgets::button(
                        theme,
                        "run",
                        "Run",
                        ButtonKind::Primary,
                        true,
                    ))
                    .pl(px(8.))
                    .child(kbd_on_accent(theme, "⌘↵"))
                    .on_click(cx.listener(|this, _, w, cx| this.run(RunScope::Statement, w, cx))),
                );
        }
        bar
    }

    fn render_filter_bar(
        &self,
        browse: &Browse,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let sorted = browse.order.as_ref().map(|(column, direction)| {
            let arrow = match direction {
                SortDirection::Ascending => "▲",
                SortDirection::Descending => "▼",
            };
            format!("order by {column} {arrow}")
        });
        let active = sorted.is_some() || !browse.filter.read(cx).value().trim().is_empty();

        div()
            .h(px(44.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(8.))
            .border_b_1()
            .border_color(theme.line)
            .child(
                field(
                    theme,
                    Some(
                        div()
                            .font_family(MONO_FONT)
                            .text_size(px(10.5))
                            .text_color(if self.field_focus.browse {
                                theme.acc
                            } else {
                                theme.t3
                            })
                            .child("WHERE")
                            .into_any_element(),
                    ),
                    &browse.filter,
                    self.field_focus.browse,
                )
                .flex_1(),
            )
            .when_some(sorted, |el, sorted| {
                el.child(
                    div()
                        .font_family(MONO_FONT)
                        .text_size(px(10.5))
                        .text_color(theme.t2)
                        .child(sorted),
                )
            })
            .when(active, |el| {
                el.child(
                    widgets::button(theme, "clear-filter", "Reset", ButtonKind::Ghost, true)
                        .on_click(cx.listener(|this, _, w, cx| this.clear_browse_filter(w, cx))),
                )
            })
    }

    fn render_results_bar(&self, theme: &Theme) -> impl IntoElement {
        let tab = self.active_tab();
        // A status dot and word, then quieter facts about the result.
        let (dot, status): (Rgba, SharedString) = match &tab.query {
            Query::Idle => (theme.t3, "No results".into()),
            Query::Running { .. } => (theme.acc, "Running…".into()),
            Query::Failed(_) => (theme.err, "Error".into()),
            Query::Done(result) => (
                theme.ok,
                match (&result.outcome, &tab.grid) {
                    (QueryOutcome::Rows(_), Some(grid)) => {
                        let count = group_digits(grid.data.rows.len());
                        let noun = if grid.data.rows.len() == 1 {
                            "row"
                        } else {
                            "rows"
                        };
                        if grid.data.truncated {
                            format!("First {count} {noun}")
                        } else {
                            format!("{count} {noun}")
                        }
                    }
                    (QueryOutcome::Affected(n), _) => {
                        format!("{} rows affected", group_digits(*n as usize))
                    }
                    _ => "Done".into(),
                }
                .into(),
            ),
        };
        let mut facts: Vec<SharedString> = Vec::new();
        if let Query::Done(result) = &tab.query {
            facts.push(format_duration(result.elapsed).into());
            if result.statements > 1 {
                facts.push(format!("{} statements", result.statements).into());
            }
        }
        if let Query::Running { .. } = &tab.query {
            facts.push("⌘. to cancel".into());
        }

        div()
            .h(px(32.))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.))
            .px(px(14.))
            .border_b_1()
            .border_color(theme.line)
            .bg(theme.s1)
            .text_size(px(11.5))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .text_color(theme.text)
                    .font_weight(FontWeight::MEDIUM)
                    .child(div().size(px(7.)).rounded_full().bg(dot))
                    .child(status),
            )
            .children(facts.into_iter().map(|fact| {
                div()
                    .font_family(MONO_FONT)
                    .text_size(px(11.))
                    .text_color(theme.t3)
                    .child(fact)
            }))
    }

    fn render_results(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let tab = self.active_tab();
        if let Some(grid) = &tab.grid {
            if grid.data.columns.is_empty() {
                return centered_hint(theme, "The query returned no columns.");
            }
            let widths = results::layout(&grid.natural, &tab.manual_widths(grid));
            let sort = match &tab.browse {
                Some(browse) => browse.order.as_ref().and_then(|(name, dir)| {
                    grid.data
                        .columns
                        .iter()
                        .position(|c| &c.name == name)
                        .map(|ix| (ix, *dir))
                }),
                None => grid.sort,
            };

            let this = cx.entity().downgrade();
            let tab_id = tab.id;
            let callbacks = results::Callbacks {
                select: Rc::new({
                    let this = this.clone();
                    move |row, column, extend, window, cx| {
                        this.update(cx, |this, cx| {
                            this.select_cell(row, column, extend, window, cx)
                        })
                        .ok();
                    }
                }),
                sort: Rc::new({
                    let this = this.clone();
                    move |column, window, cx| {
                        this.update(cx, |this, cx| this.sort_column(column, window, cx))
                            .ok();
                    }
                }),
                resize_start: Rc::new({
                    let this = this.clone();
                    move |column, x, width, _, cx| {
                        this.update(cx, |this, _| {
                            let Some(tab) = this.tabs.iter().find(|t| t.id == tab_id) else {
                                return;
                            };
                            let Some(grid) = &tab.grid else { return };
                            this.drag = Some(Drag::Column {
                                tab_id,
                                column: grid.data.columns[column].name.clone(),
                                start_x: x,
                                start_width: width,
                            });
                        })
                        .ok();
                    }
                }),
                autofit: Rc::new({
                    move |column, _, cx| {
                        this.update(cx, |this, cx| {
                            let Some(tab) = this.tabs.iter_mut().find(|t| t.id == tab_id) else {
                                return;
                            };
                            let Some(grid) = &tab.grid else { return };
                            let name = grid.data.columns[column].name.clone();
                            let natural = grid.natural[column];
                            tab.column_widths.insert(name, natural);
                            cx.notify();
                        })
                        .ok();
                    }
                }),
            };

            let view = results::View {
                grid,
                widths,
                selection: tab.selection,
                sort,
            };
            return div()
                .id("results")
                .key_context("ResultsGrid")
                .track_focus(&self.grid_focus)
                .size_full()
                .child(results::render(view, theme, callbacks))
                .into_any_element();
        }

        match &tab.query {
            Query::Failed(message) => div()
                .m(px(14.))
                .px(px(14.))
                .py(px(12.))
                .flex()
                .gap(px(10.))
                .rounded(px(widgets::RADIUS_CARD))
                .bg(Rgba {
                    a: 0.08,
                    ..theme.err
                })
                .border_1()
                .border_color(Rgba {
                    a: 0.3,
                    ..theme.err
                })
                .child(
                    svg()
                        .path(assets::icon("triangle-alert"))
                        .mt(px(1.))
                        .size(px(15.))
                        .flex_none()
                        .text_color(theme.err),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .font_family(MONO_FONT)
                        .text_size(px(11.5))
                        .line_height(px(18.))
                        .text_color(theme.text)
                        .child(message.clone()),
                )
                .into_any_element(),
            Query::Running { .. } => centered_hint(theme, "Running…"),
            Query::Done(_) => centered_hint(theme, "Done."),
            Query::Idle if self.active_session().is_some() => centered_hint(
                theme,
                "⌘↵ runs the statement under the cursor, ⇧⌘↵ runs everything.",
            ),
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
                        .child("Open a SQLite file (⌘O) or connect to Postgres (⌘N)."),
                )
                .into_any_element(),
        }
    }

    fn render_inspector(&self, theme: &Theme) -> impl IntoElement {
        let tab = self.active_tab();
        let mut panel = div()
            .w(px(INSPECTOR_WIDTH))
            .flex_none()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(theme.line);

        let selected = tab
            .selection
            .zip(tab.grid.as_ref())
            .and_then(|(selection, grid)| {
                let ix = selection.head.0;
                (ix < grid.row_count()).then(|| (ix, grid.row(ix), &grid.data.columns))
            });
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
                    .px(px(12.))
                    .py(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .border_b_1()
                    .border_color(theme.line_2)
                    .font_family(MONO_FONT)
                    .text_size(px(11.5))
                    .child(
                        div()
                            .font_family(UI_FONT)
                            .text_size(px(11.))
                            .text_color(theme.t3)
                            .child(SharedString::from(column.name.clone())),
                    )
                    .child(if value.is_null() {
                        div().italic().text_color(theme.t3).child("NULL")
                    } else {
                        let text = value.display();
                        let shown = pretty_json(&text).unwrap_or_else(|| text.into_owned());
                        div().child(SharedString::from(shown))
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

        bar = match self.active_session() {
            Some(session) => bar
                .child(
                    cell("".into(), theme.t2)
                        .gap(px(6.))
                        .child(div().size(px(6.)).rounded_full().bg(theme.ok))
                        .child(session.name.clone())
                        .children(
                            connection_tags(
                                theme,
                                session.environment,
                                session.read_only,
                                session.conn.is_local(),
                            )
                            .into_iter()
                            .map(|(label, color)| env_tag(label, color)),
                        ),
                )
                .child(cell(session.version.clone().into(), theme.t3))
                .when_some(session.conn.tunnel(), |bar, via| {
                    bar.child(cell(format!("ssh via {via}").into(), theme.t3))
                })
                .child(cell(
                    if session.read_only {
                        "read-only"
                    } else {
                        "auto-commit"
                    }
                    .into(),
                    theme.t3,
                )),
            None => bar.child(cell("no connection".into(), theme.t3)),
        };
        let open = self.sessions.len();
        bar = bar.when(open > 1, |bar| {
            bar.child(div().flex_1())
                .child(cell(format!("{open} connections open").into(), theme.t3).border_l_1())
        });

        bar.when(open <= 1, |bar| bar.child(div().flex_1())).child(
            cell(
                concat!("xenon ", env!("CARGO_PKG_VERSION")).into(),
                theme.t3,
            )
            .border_l_1(),
        )
    }
}
