//! The main window: title bar with query tabs, icon rail, side panel (explorer
//! or history), editor and results, inspector, status bar.
//! Layout follows PLAN.md, "Design direction".

use std::cell::{Cell, RefCell};
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
use xenon_driver::sql::{Risk, SortDirection, assess, statement_at};
use xenon_driver::{
    CancelHandle, Connection, ObjectKind, QueryError, QueryOutcome, QueryResult, Schema, TableInfo,
    Value,
};
use xenon_driver::{ConnectOptions, SslMode};
use xenon_sqlite::SqliteConnection;
use xenon_store::{
    Environment, HistoryEntry, NewHistoryEntry, SavedConnection, ServerEngine, Store, Target,
    secrets,
};

use crate::assets;
use crate::completion::{Catalog, SqlCompletions};
use crate::confirm::{ConfirmDialog, ConfirmEvent};
use crate::connect::{ConnectDialog, ConnectEvent, Request, RequestTarget, Subject};
use crate::detect::{Detected, detect};
use crate::discover::{Found, FoundTarget};
use crate::edits::{self, Edits, RowRef, TableRef};
use crate::palette::{self, Palette, PaletteEvent};
use crate::results::{self, Grid, Selection};
use crate::servers;
use crate::settings::{Prefs, SettingsDialog, SettingsEvent, ThemeChoice};
use crate::theme::{self, ActiveTheme, Layout, Mode, Theme};
use crate::updates::UpdateState;
use crate::widgets::{
    self as widgets, ButtonKind, Engine, RADIUS_CONTROL, centered_hint, connection_tags,
    empty_hint, env_tag, field, format_duration, group_digits, kbd, kbd_on_accent, panel_label,
    pretty_json,
};
use crate::{
    AddRow, CancelQuery, CheckForUpdates, CloseConnection, CloseTab, CopyCsv, CopyJson,
    CopyMarkdown, CopySelection, CopySql, DeleteRows, DiscardEdits, EditCell, Explain,
    ExportResults, ExtendDown, ExtendLeft, ExtendRight, ExtendUp, FocusEditor, ImportConnections,
    MoveDown, MoveLeft, MoveRight, MoveUp, NewConnection, NewTab, NextTab, OpenFile, OpenPalette,
    OpenSettings, PrevTab, Quit, RunAll, RunQuery, SaveEdits, SaveQuery, SaveQueryAs,
    SelectAllCells, ShowConnections, ShowExplorer, ShowHistory, ShowSaved, ToggleExplorer,
    ToggleInspector, ToggleStructure, UseDarkTheme, UseLightTheme, UseSystemTheme, ZoomIn, ZoomOut,
    ZoomReset,
};

const TITLEBAR_HEIGHT: f32 = 34.;
const STATUSBAR_HEIGHT: f32 = 24.;
const RAIL_WIDTH: f32 = 48.;
const SIDE_PANEL_WIDTH: f32 = 240.;
const MIN_SIDE_PANEL_WIDTH: f32 = 180.;
const MAX_SIDE_PANEL_WIDTH: f32 = 520.;
const INSPECTOR_WIDTH: f32 = 260.;
const MIN_INSPECTOR_WIDTH: f32 = 200.;
const MAX_INSPECTOR_WIDTH: f32 = 640.;
/// The results keep at least this much width when the inspector widens.
const MIN_RESULTS_WIDTH: f32 = 680.;
/// Narrower than this, the results bar's controls go compact.
const ROOMY_RESULTS: f32 = 1_000.;
const EDITOR_HEIGHT: f32 = 220.;
const MIN_EDITOR_HEIGHT: f32 = 72.;
/// Space always left for the results below a dragged editor.
const MIN_RESULTS_HEIGHT: f32 = 180.;
/// What totals were worked out for: the rows, their order, and the selection.
type StatsKey = (usize, usize, Selection);

/// What a failed query got wrong: where, and what was likely meant.
pub(crate) struct ErrorHint {
    /// The range in the editor it points at, when it's known.
    pub at: Option<std::ops::Range<usize>>,
    pub message: SharedString,
    /// The table or column most likely meant, for a misspelt one.
    pub suggestion: Option<SharedString>,
    /// The misspelt name the suggestion replaces, as it stands at `at`.
    pub replaces: Option<String>,
}

/// Results kept from one script; later ones are left out.
const MAX_RESULT_SETS: usize = 50;
/// The space between the cards the window is laid out in.
const CARD_GAP: f32 = 8.;
const CARD_RADIUS: f32 = 12.;
/// Seconds the side panel takes to slide open or shut.
const SIDE_PANEL_SLIDE: f32 = 0.18;
const TAB_MAX_WIDTH: f32 = 240.;
/// Rows from which sorting by an unindexed column asks first.
const LARGE_TABLE: u64 = 1_000_000;
/// Room for the macOS traffic lights in the transparent title bar.
const TRAFFIC_LIGHT_INSET: f32 = 78.;

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
    /// What the editor completes against; shared with every tab on this
    /// connection, and replaced when the schema changes.
    pub catalog: Rc<RefCell<Option<Rc<Catalog>>>>,
    /// A transaction the user began is open (its locks held until it ends).
    pub in_transaction: bool,
    /// The server's TLS certificate is checked (`verify-ca` / `verify-full`),
    /// or it's a file.
    pub tls_verified: bool,
    /// How it was opened, to open it again if it drops.
    pub(crate) profile: Profile,
    /// When it last ran something; one idle for long is checked before use.
    pub(crate) last_used: std::time::Instant,
    /// Checking it still answers, or opening it again.
    pub(crate) reconnecting: bool,
}

/// Everything needed to open (and maybe remember) a connection.
#[derive(Clone)]
pub(crate) struct Profile {
    pub(crate) saved_id: Option<i64>,
    /// Remember this connection after it opens.
    save: bool,
    name: String,
    pub(crate) environment: Environment,
    read_only: bool,
    /// Its colour (see `widgets::CONNECTION_COLORS`), or empty.
    pub(crate) color: String,
    pub(crate) target: RequestTarget,
    /// Set from Settings for production connections when connecting.
    lock_timeout: Option<std::time::Duration>,
    /// Submitted from the connection form, which shows the saved passwords: an
    /// empty one there was cleared on purpose. Everywhere else an empty
    /// password means "use the Keychain's".
    edited: bool,
}

impl Profile {
    pub(crate) fn from_saved(saved: &SavedConnection) -> Self {
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
                tls,
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
                    address: None,
                    ssh: ssh.as_ref().map(servers::ssh_options),
                    lock_timeout: None,
                    tls: servers::tls_files(tls),
                },
            ),
        };
        Self {
            saved_id: Some(saved.id),
            save: true,
            name: saved.name.clone(),
            environment: saved.environment,
            read_only: saved.read_only,
            color: saved.color.clone(),
            target,
            lock_timeout: None,
            edited: false,
        }
    }

    fn from_request(request: Request) -> Self {
        Self {
            saved_id: (request.id != 0).then_some(request.id),
            save: request.save,
            name: request.name,
            environment: request.environment,
            read_only: request.read_only,
            color: request.color,
            target: request.target,
            lock_timeout: None,
            edited: true,
        }
    }

    /// Runs on a background thread.
    pub(crate) fn connect(&self) -> anyhow::Result<Arc<dyn Connection>> {
        Ok(match &self.target {
            RequestTarget::Sqlite(path) => {
                let path = Path::new(path);
                if xenon_duckdb::FileKind::of(path).is_some() {
                    Arc::new(xenon_duckdb::DuckDbConnection::open(path, self.read_only)?)
                } else {
                    Arc::new(SqliteConnection::open(path, self.read_only)?)
                }
            }
            RequestTarget::Server(engine, options) => {
                let mut options = options.clone();
                options.read_only = self.read_only;
                options.lock_timeout = self.lock_timeout;
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

    /// Where this connection goes, as it's saved.
    fn saved_target(&self) -> Target {
        match &self.target {
            RequestTarget::Sqlite(path) => Target::Sqlite { path: path.clone() },
            RequestTarget::Server(engine, options) => Target::Server {
                engine: *engine,
                host: options.host.clone(),
                port: options.port,
                database: options.database.clone(),
                user: options.user.clone(),
                ssl: options.ssl.as_str().to_string(),
                ssh: options.ssh.as_ref().map(servers::ssh_tunnel),
                tls: servers::tls_paths(&options.tls),
            },
        }
    }

    /// Save (or update) this connection and its password. Runs on a background thread.
    fn remember(&self, store: Option<&Store>) -> anyhow::Result<Option<i64>> {
        let (Some(store), true) = (store, self.save) else {
            return Ok(self.saved_id);
        };
        let id = store.save_connection(&SavedConnection {
            id: self.saved_id.unwrap_or(0),
            name: self.name.clone(),
            target: self.saved_target(),
            environment: self.environment,
            read_only: self.read_only,
            color: self.color.clone(),
        })?;
        // From the form, which shows the saved passwords, what it sends is what
        // to keep: one cleared there (or a tunnel removed) leaves the Keychain.
        // Opened any other way, an empty password means "the Keychain's": keep it.
        if let RequestTarget::Server(_, options) = &self.target {
            let ssh = options.ssh.as_ref().map_or("", |ssh| ssh.password.as_str());
            for (key, secret) in [
                (SavedConnection::secret_key(id), options.password.as_str()),
                (SavedConnection::ssh_secret_key(id), ssh),
            ] {
                if self.edited || !secret.is_empty() {
                    secrets::set(&key, secret)?;
                }
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
    pub saved: bool,
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
        Detected::File(path) => format!("SQLite file · {} · press Enter", path.display()),
        Detected::NotYet(engine) => {
            return (format!("{engine} · coming soon").into(), true);
        }
        Detected::Unknown => String::new(),
    };
    (note.into(), false)
}

/// An action waiting on the confirmation dialog.
pub(crate) enum Pending {
    Run {
        tab_id: usize,
        sql: String,
        origin: Option<usize>,
        row_limit: Option<usize>,
    },
    Forget(i64),
    DeleteQuery(i64),
    /// Disconnect a session with a transaction open, or unsaved changes.
    Disconnect(usize),
    /// Close a tab with unsaved changes.
    CloseTab(usize),
    /// Sort a large table by a column without an index.
    Sort {
        tab_id: usize,
        column: usize,
    },
    /// Cancel a SQL Server query, rolling back its open transaction.
    Cancel(usize),
    /// Quit with a transaction open.
    Quit,
    /// Save a tab's staged changes.
    Save {
        tab_id: usize,
        statements: Vec<edits::Statement>,
    },
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
pub(crate) enum SidePanel {
    Explorer,
    History,
    Saved,
}

pub(crate) enum Query {
    Idle,
    Running {
        /// Which run this is, so a slow poller can't touch a newer one.
        run: u64,
        cancel: Arc<dyn CancelHandle>,
        _task: Task<()>,
        _poll: Task<()>,
    },
    Failed(SharedString),
    Done(Arc<QueryResult>),
}

/// A tab opened by browsing a table: its query is generated from a filter and
/// a sort, so header clicks sort on the server rather than in the grid.
pub(crate) struct Browse {
    pub(crate) schema: String,
    pub(crate) table: String,
    pub(crate) filter: Entity<InputState>,
    order: Option<(String, SortDirection)>,
    _filter_enter: Subscription,
    /// The filter the rows were last loaded with.
    pub(crate) applied: String,
    /// The SQL the table was last loaded with. Rows are saved to the table only
    /// while they come from it: other SQL run in the tab (typed, or from
    /// History) shows rows that aren't this table's.
    pub(crate) loaded_with: Option<String>,
    /// Which page of rows is shown, from 0, and the filter and sort it's a page
    /// of (a new one starts again at the first).
    pub(crate) page: usize,
    paged_for: Option<(String, Option<(String, SortDirection)>)>,
    /// How many rows there are in all.
    pub(crate) total: Option<crate::pages::Total>,
    /// Columns, keys and indexes, loaded when the tab opens.
    pub(crate) info: Option<Rc<TableInfo>>,
    /// The filter builder, when open.
    pub(crate) builder: Option<crate::filter::Builder>,
    info_error: Option<SharedString>,
    /// Show the table's structure instead of its rows.
    pub(crate) structure: bool,
    /// Its definition in a read-only editor, and the text it holds.
    definition: Option<(String, Entity<EditorState>)>,
}

impl Browse {
    /// The single-column foreign key a column belongs to.
    fn key_for(&self, column: &str) -> Option<&xenon_driver::ForeignKey> {
        self.info
            .as_ref()?
            .foreign_keys
            .iter()
            .find(|key| key.single().is_some_and(|(from, _)| from == column))
    }
}

impl Browse {
    fn key(&self) -> String {
        format!("{}.{}", self.schema, self.table)
    }
}

/// One query tab: its own editor and its own results.
pub(crate) struct Tab {
    pub(crate) id: usize,
    /// The connection this tab runs against; `None` shows the Connect screen.
    pub(crate) session: Option<usize>,
    pub(crate) title: SharedString,
    pub(crate) browse: Option<Browse>,
    /// The saved query this tab holds, so ⌘S updates it.
    pub(crate) saved_query: Option<i64>,
    pub(crate) editor: Entity<EditorState>,
    pub(crate) query: Query,
    pub(crate) grid: Option<Grid>,
    pub(crate) selection: Option<Selection>,
    /// Widths the user dragged, by column name; kept across re-runs.
    column_widths: HashMap<String, f32>,
    /// Byte offset in the editor where the last run's SQL began, to place
    /// error positions; `None` when the SQL ran was rewritten (Explain).
    run_origin: Option<usize>,
    /// What the last failed query got wrong, to point at and offer to fix.
    pub(crate) error_hint: Option<ErrorHint>,
    /// Changes to the table's rows, not yet saved.
    pub(crate) edits: Rc<Edits>,
    /// The cell open in the edit bar.
    editing: Option<CellEditor>,
    /// A short note in the results bar (saved, or why something can't happen), and whether it's an error.
    pub(crate) flash: Option<(SharedString, bool)>,
    pub(crate) saving: bool,
    /// The last save's commit may or may not have happened (the connection
    /// dropped): saving again could apply the edits twice, so not until the
    /// table is reloaded.
    pub(crate) save_unsure: bool,
    /// Rows read so far by the running query.
    fetched: usize,
    /// The grid shows the running query's first rows.
    previewing: bool,
    /// Every result with rows from the last script, when it returned several:
    /// one slot each, empty for the one in `grid`.
    pub(crate) sets: Vec<Option<Grid>>,
    /// Which of `sets` is in `grid`.
    pub(crate) shown_set: usize,
    /// How many result sets the script returned, kept or not (only the first
    /// and the last are kept past `MAX_RESULT_SETS`).
    pub(crate) sets_total: usize,
    /// The columns' arrangement (hidden, pinned, moved), kept across runs.
    pub(crate) columns: Option<results::ColumnLayout>,
    /// Rows as a grid, as records, or as JSON.
    pub(crate) view: crate::views::ResultView,
    pub(crate) json: Option<crate::views::JsonView>,
    /// The JSON view on one line per row instead of laid out.
    pub(crate) json_compact: bool,
    /// Set when the production time limit stopped the running query.
    timed_out: Option<std::time::Duration>,
    /// The last SQL run here and where it began in the editor, for "Fetch all".
    last_run: Option<(String, Option<usize>)>,
}

/// What a running query has read so far, shared with the thread running it.
#[derive(Default)]
pub(crate) struct Live {
    preview: std::sync::Mutex<Option<xenon_driver::ResultSet>>,
    rows: std::sync::atomic::AtomicUsize,
    pub(crate) done: std::sync::atomic::AtomicBool,
    /// The production time limit stopped it.
    timed_out: std::sync::atomic::AtomicBool,
}

/// Cancel a query that runs past `limit`, from its own thread, so the limit
/// holds whatever the window is doing. Ends when the query does.
pub(crate) fn watchdog(live: Arc<Live>, cancel: Arc<dyn CancelHandle>, limit: std::time::Duration) {
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + limit;
        while !live.done.load(std::sync::atomic::Ordering::SeqCst) {
            if std::time::Instant::now() >= deadline {
                live.timed_out
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                cancel.cancel();
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    });
}

/// The edit bar's cell and its text field.
struct CellEditor {
    row: RowRef,
    column: usize,
    /// Which editor the value gets (line, text area, JSON, date, choice…).
    kind: crate::value_editor::ValueKind,
    input: Entity<InputState>,
    /// The text area, for long text and JSON.
    area: Option<Entity<EditorState>>,
    _enter: Subscription,
}

impl Tab {
    /// The result's columns: name and type as reported.
    pub(crate) fn grid_columns(&self) -> Vec<(String, Option<String>)> {
        self.grid.as_ref().map_or_else(Vec::new, |grid| {
            grid.data
                .columns
                .iter()
                .map(|c| (c.name.clone(), c.type_name.clone()))
                .collect()
        })
    }

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
    /// The inspector's left edge.
    Inspector { start_x: Pixels, start_width: f32 },
}

pub struct Workspace {
    focus_handle: FocusHandle,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active: usize,
    next_tab_id: usize,
    pub(crate) sessions: Vec<Session>,
    next_session_id: usize,
    next_run: u64,
    /// A problem opening a database, shown on the Connect screen and in the explorer.
    pub(crate) notice: Option<SharedString>,
    /// The status bar's connection menu is open.
    pub(crate) switcher: bool,
    /// A header's right-click menu: which display column, and where.
    pub(crate) column_menu: Option<(usize, gpui::Point<Pixels>)>,
    /// Totals for the selected cells, kept until the selection, rows or their
    /// order change: adding up a million cells every frame would be slow.
    selection_stats: RefCell<Option<(StatsKey, Option<crate::stats::Stats>)>>,
    /// Whether a newer Xenon is out, shown in the status bar.
    pub(crate) update: crate::updates::UpdateState,
    /// The connection being opened, while it opens.
    pub(crate) connecting: Option<SharedString>,
    pub(crate) store: Option<Arc<Store>>,
    pub(crate) history: Vec<HistoryEntry>,
    pub(crate) history_filter: Entity<InputState>,
    side_panel: SidePanel,
    grid_focus: FocusHandle,
    drag: Option<Drag>,
    /// The Export format menu is open.
    pub(crate) export_menu: bool,
    /// Where the last export was saved.
    export_dir: Option<std::path::PathBuf>,
    /// A failed query's error position, applied to the editor on the next frame.
    /// A failed query's tab and the editor range to select and underline.
    pending_error_jump: Option<(usize, std::ops::Range<usize>)>,
    first_frame_traced: bool,
    bench: Option<crate::bench::Bench>,
    bench_pending_sql: Option<String>,
    bench_frame_requested: bool,
    editor_height: f32,
    side_panel_width: f32,
    pub(crate) inspector_width: f32,
    /// How wide the results are, worked out each frame: narrow, the bar's
    /// controls go compact.
    results_width: Cell<f32>,
    /// The inspector's width as drawn: its own, unless the window is too narrow
    /// to leave the results their room.
    inspector_shown: Cell<f32>,
    /// Which text fields have focus, refreshed each render, for focus rings.
    pub(crate) field_focus: FieldFocus,
    /// The launcher's "paste anything" field and what it recognised (text, is_error).
    pub(crate) paste_input: Entity<InputState>,
    pub(crate) paste_note: (SharedString, bool),
    /// Databases found on this Mac, and in a scanned project's .env files.
    pub(crate) found: Vec<Found>,
    pub(crate) project_found: Vec<Found>,
    pub(crate) discovering: bool,
    pub(crate) prefs: Prefs,
    pub(crate) settings: Option<(Entity<SettingsDialog>, Subscription)>,
    pub(crate) palette: Option<(Entity<Palette>, Subscription)>,
    pub(crate) connect_dialog: Option<(Entity<ConnectDialog>, Subscription)>,
    pub(crate) confirm: Option<(Entity<ConfirmDialog>, Subscription, Pending)>,
    pub(crate) saved: Vec<SavedConnection>,
    /// Folders of saved connections or queries that are folded (`connections/Clients`).
    pub(crate) folded_folders: HashSet<String>,
    pub(crate) explorer_filter: Entity<InputState>,
    show_side_panel: bool,
    /// The mouse went down on empty title bar space: moving it drags the window.
    titlebar_pressed: bool,
    /// When the side panel last opened or closed, to slide it.
    side_panel_since: Option<std::time::Instant>,
    show_inspector: bool,
    /// When the inspector last opened or closed, to slide it.
    inspector_since: Option<std::time::Instant>,
    pub(crate) _subscriptions: Vec<Subscription>,
    /// The Saved panel: queries matching its filter.
    pub(crate) saved_queries: Vec<xenon_store::SavedQuery>,
    pub(crate) saved_filter: Entity<InputState>,
    /// Asking for a name before saving a query.
    pub(crate) naming: Option<crate::saved::Naming>,
    /// File → Import Connections…, when open.
    pub(crate) import: Option<crate::import::Import>,
    /// Asking for a query's parameters before it runs.
    pub(crate) parameters: Option<crate::params::Prompt>,
    /// Last session's tabs, by saved connection, waiting for it to connect.
    pub(crate) restoring: HashMap<i64, Vec<xenon_store::OpenTab>>,
    /// Launched with a file or URL: last session's tabs are left as saved.
    pub(crate) keep_saved_tabs: bool,
    /// The tabs last written to the store.
    pub(crate) persisted_tabs: Vec<xenon_store::OpenTab>,
}

impl Workspace {
    pub fn new(launch: Option<Launch>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let appearance = cx.observe_window_appearance(window, |this: &mut Self, window, cx| {
            if this.prefs.theme == ThemeChoice::System {
                theme::apply(Mode::from_appearance(window.appearance()), cx);
                cx.notify();
            }
        });

        let history_filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter history"));
        let saved_filter =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter saved queries"));
        let saved_changes = cx.subscribe(&saved_filter, |this, _, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.reload_saved_queries(cx);
            }
        });
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
        let prefs = Prefs::load(store.as_deref());
        theme::set_accent(prefs.accent, cx);
        theme::set_layout(prefs.layout, cx);
        theme::set_type(
            prefs.ui_font,
            prefs.code_font,
            prefs.zoom,
            prefs.editor_font,
        );
        theme::set_code_theme(prefs.code_theme);
        theme::apply(prefs.theme.mode(window), cx);
        window.set_background_appearance(window_material(prefs.frosted));

        let mut this = Self {
            focus_handle: cx.focus_handle(),
            tabs: Vec::new(),
            active: 0,
            next_tab_id: 0,
            sessions: Vec::new(),
            next_session_id: 0,
            next_run: 0,
            notice: None,
            update: Default::default(),
            selection_stats: RefCell::new(None),
            column_menu: None,
            switcher: false,
            connecting: None,
            store,
            history: Vec::new(),
            history_filter,
            side_panel: SidePanel::Explorer,
            grid_focus: cx.focus_handle(),
            drag: None,
            export_menu: false,
            export_dir: None,
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
            inspector_width: INSPECTOR_WIDTH,
            results_width: Cell::new(f32::MAX),
            inspector_shown: Cell::new(INSPECTOR_WIDTH),
            field_focus: FieldFocus::default(),
            paste_input,
            paste_note: (SharedString::default(), false),
            found: Vec::new(),
            project_found: Vec::new(),
            discovering: false,
            prefs,
            settings: None,
            palette: None,
            connect_dialog: None,
            confirm: None,
            saved: Vec::new(),
            folded_folders: HashSet::new(),
            explorer_filter,
            // Closed on the connect screen; it opens with the first connection.
            show_side_panel: false,
            titlebar_pressed: false,
            side_panel_since: None,
            show_inspector: true,
            inspector_since: None,
            _subscriptions: vec![
                appearance,
                filter_changes,
                explorer_changes,
                paste_events,
                saved_changes,
            ],
            saved_queries: Vec::new(),
            saved_filter,
            naming: None,
            parameters: None,
            import: None,
            restoring: HashMap::new(),
            keep_saved_tabs: false,
            persisted_tabs: Vec::new(),
        };
        this.reload_saved();
        this.rediscover(cx);
        this.add_tab(None, None, window, cx);
        // Deferred so it runs after gpui-component's Root has been built around us.
        cx.defer_in(window, |this: &mut Self, window, cx| {
            this.focus_editor(window, cx)
        });

        this.reload_saved_queries(cx);
        // Launched to open something: last session's tabs weren't brought back,
        // so this run's don't replace them.
        this.keep_saved_tabs = launch.is_some();
        match launch {
            Some(Launch::Sqlite(path)) => this.open_path(path, window, cx),
            Some(Launch::Url(engine, url)) => this.connect_url(engine, url, window, cx),
            None if this.prefs.restore_tabs => this.restore_tabs(window, cx),
            None => {}
        }
        this.watch_tabs(cx);
        cx.set_global(QuitGuard(cx.entity().downgrade()));
        // Closing the window quits: ask first when a transaction is open.
        let workspace = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            workspace
                .update(cx, |this, cx| this.may_quit(window, cx))
                .unwrap_or(true)
        });
        this.check_for_updates_at_launch(cx);
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

    pub(crate) fn session_mut(&mut self, id: usize) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    /// The connection the active tab runs against.
    pub(crate) fn active_session(&self) -> Option<&Session> {
        self.active_tab().session.and_then(|id| self.session(id))
    }

    pub(crate) fn add_tab(
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
                // ⌘F finds, ⇧⌘F replaces.
                .searchable(true)
                .placeholder("-- Write SQL here")
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
                        applied: String::new(),
                        loaded_with: None,
                        page: 0,
                        paged_for: None,
                        total: None,
                        info: None,
                        info_error: None,
                        structure: false,
                        definition: None,
                        builder: None,
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
            error_hint: None,
            edits: Rc::default(),
            editing: None,
            flash: None,
            saving: false,
            save_unsure: false,
            fetched: 0,
            previewing: false,
            sets: Vec::new(),
            shown_set: 0,
            sets_total: 0,
            view: Default::default(),
            json: None,
            json_compact: false,
            columns: None,
            last_run: None,
            saved_query: None,
            timed_out: None,
        });
        self.active = self.tabs.len() - 1;
        self.hook_completions(self.active, cx);
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

    pub(crate) fn activate_tab(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
    /// Point a tab's editor at its connection's schema for autocomplete.
    fn hook_completions(&self, tab_index: usize, cx: &mut Context<Self>) {
        let tab = &self.tabs[tab_index];
        let Some(session) = tab.session.and_then(|id| self.session(id)) else {
            return;
        };
        let provider = Rc::new(SqlCompletions {
            catalog: session.catalog.clone(),
        });
        tab.editor.update(cx, |editor, _| {
            editor.lsp_mut().completion_provider = Some(provider);
        });
    }

    pub(crate) fn attach(
        &mut self,
        id: usize,
        existing: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_tab().session.is_none() {
            let number = self.free_query_number();
            let tab = &mut self.tabs[self.active];
            tab.session = Some(id);
            tab.title = format!("Query {number}").into();
            self.hook_completions(self.active, cx);
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

    pub(crate) fn close_tab_at(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Staged changes would go with it: ask.
        let tab = &self.tabs[index];
        if !tab.edits.is_empty() {
            let body = format!(
                "{} has {} not saved. Closing it discards them.",
                tab.title,
                changes(tab.edits.count())
            );
            let tab_id = tab.id;
            let dialog = cx.new(|cx| {
                ConfirmDialog::new("Discard changes?", body, "", None, window, cx)
                    .action("Discard and close", true)
            });
            self.show_confirm(dialog, Pending::CloseTab(tab_id), window, cx);
            return;
        }
        self.close_tab_now(index, window, cx);
    }

    fn close_tab_now(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        // A tab's query stops with it, except on SQL Server inside a transaction,
        // where stopping it would end the session and roll the transaction back.
        let ends_session = self.tabs[index]
            .session
            .and_then(|id| self.session(id))
            .is_some_and(|s| s.engine == Engine::SqlServer && s.in_transaction);
        if let Query::Running { cancel, .. } = &self.tabs[index].query
            && !ends_session
        {
            cancel.cancel();
        }
        let showing = self.active_tab().session;
        let session = self.tabs.remove(index).session;
        if self.tabs.is_empty() {
            // There's always one tab; closing the last gives a clean "Query 1".
            self.add_tab(None, session, window, cx);
        } else if self.active >= self.tabs.len() || index < self.active {
            self.active = self.active.saturating_sub(1).min(self.tabs.len() - 1);
        }
        // History follows the connection now in front.
        if self.active_tab().session != showing {
            self.reload_history(cx);
        }
        self.focus_editor(window, cx);
        cx.notify();
    }

    pub(crate) fn new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.active_tab().session;
        self.add_tab(None, session, window, cx);
        self.focus_editor(window, cx);
    }

    pub(crate) fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        // Typed inside a dialog, it's not for the tab behind it.
        if self.modal_open() {
            return;
        }
        self.close_tab_at(self.active, window, cx);
    }

    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_tab((self.active + 1) % self.tabs.len(), window, cx);
    }

    fn prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        let len = self.tabs.len();
        self.activate_tab((self.active + len - 1) % len, window, cx);
    }

    pub(crate) fn focus_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
                color: String::new(),
                target: RequestTarget::Sqlite(path_text),
                lock_timeout: None,
                edited: false,
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
            Ok(options) => {
                let profile = self.profile_for_url(engine, options);
                self.open_profile(profile, window, cx)
            }
            Err(err) => {
                self.notice = Some(format!("{err:#}").into());
                cx.notify();
            }
        }
    }

    /// What a URL opens. A URL for a saved database is that database, opened as
    /// saved: its production label and read-only flag, and also its TLS,
    /// certificates and tunnel, so the saved password only ever goes the saved
    /// way. Only a password written in the URL is taken from it.
    pub(crate) fn profile_for_url(&self, engine: ServerEngine, options: ConnectOptions) -> Profile {
        if let Some(saved) = self.saved_for(engine, &options) {
            let mut profile = Profile::from_saved(saved);
            if let RequestTarget::Server(_, target) = &mut profile.target
                && !options.password.is_empty()
            {
                target.password = options.password;
            }
            return profile;
        }
        Profile {
            saved_id: None,
            save: false,
            name: servers::default_name(engine, &options),
            environment: Environment::None,
            read_only: false,
            color: String::new(),
            target: RequestTarget::Server(engine, options),
            lock_timeout: None,
            edited: false,
        }
    }

    /// The saved connection to the same server, database and user, if any.
    pub(crate) fn saved_for(
        &self,
        engine: ServerEngine,
        options: &ConnectOptions,
    ) -> Option<&SavedConnection> {
        let port = options.port.unwrap_or(engine.default_port());
        self.saved.iter().find(|saved| match &saved.target {
            Target::Server {
                engine: e,
                host,
                port: p,
                database,
                user,
                ..
            } => {
                *e == engine
                    && host.eq_ignore_ascii_case(&options.host)
                    && p.unwrap_or(engine.default_port()) == port
                    && *database == options.database
                    && (user.is_empty() || options.user.is_empty() || *user == options.user)
            }
            Target::Sqlite { .. } => false,
        })
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
    fn open_profile(&mut self, mut profile: Profile, window: &mut Window, cx: &mut Context<Self>) {
        self.notice = None;
        if profile.environment == Environment::Production && self.prefs.lock_wait > 0 {
            profile.lock_timeout = Some(std::time::Duration::from_secs(self.prefs.lock_wait));
        }
        if let Some(id) = profile
            .saved_id
            .and_then(|saved| self.sessions.iter().find(|s| s.saved_id == Some(saved)))
            .map(|s| s.id)
        {
            self.connect_dialog = None;
            // Already open: nothing more of it to restore.
            if let Some(saved) = profile.saved_id {
                self.restoring.remove(&saved);
            }
            if profile.save {
                self.update_open(id, profile, cx);
            }
            self.attach(id, true, window, cx);
            return;
        }
        let store = self.store.clone();
        let restoring = profile.saved_id;
        self.connecting = Some(profile.name.clone().into());
        cx.notify();
        let task = cx.background_spawn(async move {
            let conn = profile.connect()?;
            let schemas = conn.introspect()?;
            let saved_id = profile.remember(store.as_deref())?;
            anyhow::Ok((conn, schemas, profile, saved_id))
        });
        cx.spawn_in(window, async move |this, cx| {
            let opened = task.await;
            this.update_in(cx, |this, window, cx| {
                this.connecting = None;
                match opened {
                    Ok((conn, schemas, profile, saved_id)) => {
                        this.connect_dialog = None;
                        this.reload_saved();
                        // The same database reached another way (a pasted URL for a
                        // saved connection, say): use the one already open, but only
                        // when it's the same saved connection with the same guards. A
                        // production connection never lands on a development session.
                        if let Some(id) = this
                            .sessions
                            .iter()
                            .find(|s| {
                                s.conn.id() == conn.id()
                                    && s.saved_id == saved_id
                                    && s.environment == profile.environment
                                    && s.read_only == profile.read_only
                            })
                            .map(|s| s.id)
                        {
                            if let Some(saved) = restoring {
                                this.restoring.remove(&saved);
                            }
                            this.attach(id, true, window, cx);
                            return;
                        }
                        // The same saved connection, edited while open (marked
                        // Production, made read-only, renamed): its open session
                        // takes the new settings now, not at the next reconnect.
                        if let Some(saved) = saved_id
                            && let Some(index) = this
                                .sessions
                                .iter()
                                .position(|s| s.saved_id == Some(saved) && s.conn.id() == conn.id())
                        {
                            let busy = this.sessions[index].in_transaction
                                || this.tabs.iter().any(|t| {
                                    t.session == Some(this.sessions[index].id)
                                        && (matches!(t.query, Query::Running { .. }) || t.saving)
                                });
                            let session = &mut this.sessions[index];
                            session.name = profile.name.clone();
                            session.environment = profile.environment;
                            if busy {
                                // Swapping the connection now would end what's
                                // running or roll back the open transaction.
                                if session.read_only != profile.read_only {
                                    this.notice = Some(
                                        "Read-only applies once the open transaction or running \
                                         query ends: disconnect and connect again then."
                                            .into(),
                                    );
                                }
                            } else {
                                // The new connection carries the settings the
                                // server enforces (read-only); use it.
                                session.read_only = profile.read_only;
                                session.conn = conn;
                                session.profile = profile;
                            }
                            let id = this.sessions[index].id;
                            if let Some(saved) = restoring {
                                this.restoring.remove(&saved);
                            }
                            this.attach(id, true, window, cx);
                            return;
                        }
                        this.next_session_id += 1;
                        let id = this.next_session_id;
                        if this.sessions.is_empty() {
                            this.set_side_panel(true);
                        }
                        this.sessions.push(Session {
                            id,
                            profile: profile.clone(),
                            last_used: std::time::Instant::now(),
                            reconnecting: false,
                            engine: match &profile.target {
                                RequestTarget::Sqlite(_)
                                    if conn.server_version().starts_with("duckdb") =>
                                {
                                    Engine::DuckDb
                                }
                                RequestTarget::Sqlite(_) => Engine::Sqlite,
                                RequestTarget::Server(engine, _) => {
                                    servers::logo(*engine, conn.as_ref())
                                }
                            },
                            version: conn.server_version(),
                            catalog: Rc::new(RefCell::new(Some(Rc::new(Catalog::new(
                                schemas.clone(),
                                conn.clone(),
                            ))))),
                            conn,
                            schemas,
                            name: profile.name,
                            environment: profile.environment,
                            read_only: profile.read_only,
                            saved_id,
                            expanded: HashSet::new(),
                            collapsed_schemas: HashSet::new(),
                            collapsed: false,
                            in_transaction: false,
                            tls_verified: match &profile.target {
                                RequestTarget::Sqlite(_) => true,
                                RequestTarget::Server(_, options) => {
                                    matches!(options.ssl, SslMode::VerifyFull | SslMode::VerifyCa)
                                }
                            },
                        });
                        if !this.reopen_tabs(saved_id, id, window, cx) {
                            this.attach(id, false, window, cx);
                        }
                        this.bench_start(cx);
                    }
                    Err(err) => {
                        // Its tabs can't come back this time; stop waiting for them.
                        if let Some(id) = restoring {
                            this.restoring.remove(&id);
                        }
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
            Detected::File(path) => self.open_path(path, window, cx),
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
        // Found again, but already saved (with a production label, say): open
        // the saved connection, guards and all.
        if let Some(saved) = self.saved_for(*engine, options).cloned() {
            self.connect_to_saved(&saved, window, cx);
            return;
        }
        if found.ready() {
            self.open_profile(
                Profile {
                    saved_id: None,
                    save: false,
                    name: found.detail(),
                    environment: Environment::None,
                    read_only: false,
                    color: String::new(),
                    target: RequestTarget::Server(*engine, options.clone()),
                    lock_timeout: None,
                    edited: false,
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
        // Typed inside a dialog, it's not for the tab behind it.
        if self.modal_open() {
            return;
        }
        if let Some(id) = self.active_tab().session {
            self.close_session(id, window, cx);
        }
    }

    /// Disconnect session `id` and close its tabs. With nothing left open, the
    /// window goes back to the Connect screen.
    /// Whether queries on `session` go into the history (Settings → Data).
    pub(crate) fn keeps_history(&self, session: &Session) -> bool {
        session.environment != Environment::Production || self.prefs.production_history
    }

    /// Commit or roll back the active connection's open transaction.
    pub(crate) fn end_transaction(&mut self, commit: bool, cx: &mut Context<Self>) {
        let Some(session) = self.active_session() else {
            return;
        };
        let (conn, id) = (session.conn.clone(), session.id);
        let sql = if commit { "commit" } else { "rollback" };
        let task = cx.background_spawn(async move {
            let ended = conn.execute(sql, 1).map(|_| ());
            (ended, conn.in_transaction())
        });
        cx.spawn(async move |this, cx| {
            let (ended, open) = task.await;
            this.update(cx, |this, cx| {
                if let Some(session) = this.session_mut(id) {
                    session.in_transaction = open.unwrap_or(ended.is_err());
                }
                let note = match ended {
                    Ok(()) if commit => ("Committed.".to_string(), false),
                    Ok(()) => ("Rolled back.".to_string(), false),
                    Err(err) => (format!("{err:#}"), true),
                };
                this.flash(note.0, note.1, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Disconnect, asking first when a transaction is open (disconnecting rolls it back).
    pub(crate) fn close_session(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session(id) else {
            return;
        };
        let unsaved: usize = self
            .tabs
            .iter()
            .filter(|t| t.session == Some(id))
            .map(|t| t.edits.count())
            .sum();
        if unsaved > 0 && !session.in_transaction {
            let body = format!(
                "{} not saved on {}. Disconnecting discards them.",
                changes(unsaved),
                session.name
            );
            let dialog = cx.new(|cx| {
                ConfirmDialog::new("Discard changes?", body, "", None, window, cx)
                    .action("Discard and disconnect", true)
            });
            self.show_confirm(dialog, Pending::Disconnect(id), window, cx);
            return;
        }
        if session.in_transaction {
            let body = format!(
                "A transaction is open on {}. Disconnecting rolls back everything it changed.",
                session.name
            );
            let dialog = cx.new(|cx| {
                ConfirmDialog::new("Roll back and disconnect?", body, "", None, window, cx)
                    .action("Roll back and disconnect", true)
            });
            self.show_confirm(dialog, Pending::Disconnect(id), window, cx);
            return;
        }
        self.disconnect(id, window, cx);
    }

    fn disconnect(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
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

    /// Save edits to a connection that's already open. A new name applies at once;
    /// anything else waits until it reconnects.
    /// A saved connection edited while it's open. Its name and environment
    /// (Production's prompts) apply at once. Read-only, or a new address, takes
    /// a new connection (the server keeps a session read-only): opened now when
    /// nothing is running and no transaction is open, else at the next reconnect.
    pub(crate) fn update_open(
        &mut self,
        session_id: usize,
        profile: Profile,
        cx: &mut Context<Self>,
    ) {
        let reconnect = self
            .saved
            .iter()
            .find(|saved| Some(saved.id) == profile.saved_id)
            .is_none_or(|saved| {
                saved.target != profile.saved_target() || saved.read_only != profile.read_only
            });
        let busy = self
            .sessions
            .iter()
            .any(|s| s.id == session_id && s.in_transaction)
            || self.tabs.iter().any(|t| {
                t.session == Some(session_id)
                    && (matches!(t.query, Query::Running { .. }) || t.saving)
            });
        if let Some(session) = self.sessions.iter_mut().find(|s| s.id == session_id) {
            session.name = profile.name.clone();
            session.environment = profile.environment;
        }
        let store = self.store.clone();
        let swap = reconnect && !busy;
        let task = cx.background_spawn(async move {
            profile.remember(store.as_deref())?;
            // Read-only (or the address) changed: a connection with it.
            let fresh = if swap {
                Some(profile.connect().map(|conn| (conn, profile)))
            } else {
                None
            };
            anyhow::Ok(fresh)
        });
        cx.spawn(async move |this, cx| {
            let saved = task.await;
            this.update(cx, |this, cx| {
                this.reload_saved();
                match saved {
                    Err(err) => this.notice = Some(format!("{err:#}").into()),
                    Ok(Some(Ok((conn, profile)))) => {
                        if let Some(session) = this.sessions.iter_mut().find(|s| s.id == session_id)
                        {
                            session.read_only = profile.read_only;
                            session.conn = conn;
                            session.in_transaction = false;
                            session.profile = profile;
                        }
                    }
                    Ok(Some(Err(err))) => {
                        this.notice = Some(
                            format!("Saved, but couldn't reconnect with the new settings: {err:#}")
                                .into(),
                        )
                    }
                    Ok(None) if reconnect => {
                        this.notice = Some(
                            "Saved. A query or transaction is open: disconnect and reconnect when \
                             it's done to use the new settings."
                                .into(),
                        )
                    }
                    Ok(None) => {}
                }
                cx.notify();
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
                color: String::new(),
                target: RequestTarget::Server(engine, options),
                lock_timeout: None,
                edited: false,
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

    pub(crate) fn show_confirm(
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
                        row_limit,
                    } => match this.busy(tab_id) {
                        // Something started while the prompt was open: don't overlap it.
                        Some(why) => this.flash(why, true, cx),
                        None => {
                            let limit = row_limit.unwrap_or(this.prefs.row_limit);
                            this.execute_with_limit(tab_id, sql, origin, limit, cx);
                        }
                    },
                    Pending::Save { tab_id, statements } => {
                        this.save_statements(tab_id, statements, window, cx);
                        return;
                    }
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
                    Pending::DeleteQuery(id) => this.forget_query(id, cx),
                    Pending::Disconnect(id) => {
                        this.disconnect(id, window, cx);
                        return;
                    }
                    Pending::CloseTab(tab_id) => {
                        if let Some(index) = this.tabs.iter().position(|t| t.id == tab_id) {
                            this.close_tab_now(index, window, cx);
                        }
                        return;
                    }
                    Pending::Cancel(tab_id) => {
                        if let Some(Query::Running { cancel, .. }) =
                            this.tabs.iter().find(|t| t.id == tab_id).map(|t| &t.query)
                        {
                            cancel.cancel();
                        }
                    }
                    Pending::Quit => cx.quit(),
                    Pending::Sort { tab_id, column } => {
                        if let Some(index) = this.tabs.iter().position(|t| t.id == tab_id) {
                            this.active = index;
                            this.sort_now(column, window, cx);
                        }
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
        // Typed inside a dialog, it's not for the tab behind it.
        if self.modal_open() {
            return;
        }
        // Typed while editing a cell: keep the edit, rather than re-run the
        // query and lose it.
        if self.active_tab().editing.is_some() {
            self.commit_edit(false, window, cx);
            self.flash(
                "Kept your edit: save it (⌘S) or discard it before running again.",
                false,
                cx,
            );
            return;
        }
        if !self.apply_pending_filter(window, cx) {
            self.run(RunScope::Statement, window, cx);
        }
    }

    fn run_all(&mut self, _: &RunAll, window: &mut Window, cx: &mut Context<Self>) {
        // Typed inside a dialog, it's not for the tab behind it.
        if self.modal_open() {
            return;
        }
        // Typed while editing a cell: keep the edit, rather than re-run the
        // query and lose it.
        if self.active_tab().editing.is_some() {
            self.commit_edit(false, window, cx);
            self.flash(
                "Kept your edit: save it (⌘S) or discard it before running again.",
                false,
                cx,
            );
            return;
        }
        if !self.apply_pending_filter(window, cx) {
            self.run(RunScope::All, window, cx);
        }
    }

    fn explain(&mut self, _: &Explain, window: &mut Window, cx: &mut Context<Self>) {
        // Typed inside a dialog, it's not for the tab behind it.
        if self.modal_open() {
            return;
        }
        self.run(RunScope::Explain, window, cx);
    }

    pub(crate) fn run(&mut self, scope: RunScope, window: &mut Window, cx: &mut Context<Self>) {
        // A re-run reloads the rows and would lose staged changes.
        if self.guard_edits(cx) {
            return;
        }
        if self.active_session().is_none() {
            self.notice = Some("Open a database first.".into());
            cx.notify();
            return;
        }
        let tab = &self.tabs[self.active];

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
        let tab_id = tab.id;
        if self.ask_parameters(tab_id, &sql, scope, window, cx) {
            return;
        }
        self.run_sql(tab_id, sql, origin, scope, window, cx);
    }

    /// Run SQL taken from a tab's editor (with any parameters filled in).
    pub(crate) fn run_sql(
        &mut self,
        tab_id: usize,
        sql: String,
        origin: Option<usize>,
        scope: RunScope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self
            .tabs
            .iter()
            .find(|t| t.id == tab_id)
            .and_then(|t| t.session)
            .and_then(|id| self.session(id))
        else {
            return;
        };
        // Explaining a script would run everything after its first statement.
        if scope == RunScope::Explain && xenon_driver::sql::several_statements(&sql) {
            self.flash(
                "Explain works on one statement at a time: select it first.",
                true,
                cx,
            );
            return;
        }
        let sql = match scope {
            RunScope::Explain => session.conn.explain(sql.trim().trim_end_matches(';')),
            _ => sql,
        };
        self.guard(tab_id, sql, origin, None, window, cx);
    }

    /// What the active tab last ran, for tests.
    #[cfg(test)]
    pub(crate) fn last_run_sql(&self) -> Option<String> {
        self.active_tab()
            .last_run
            .as_ref()
            .map(|(sql, _)| sql.clone())
    }

    /// The active tab's note (beside the results), for tests.
    #[cfg(test)]
    pub(crate) fn flash_text(&self) -> Option<String> {
        self.active_tab()
            .flash
            .as_ref()
            .map(|(text, _)| text.to_string())
    }

    #[cfg(test)]
    pub(crate) fn is_running(&self) -> bool {
        matches!(self.active_tab().query, Query::Running { .. })
    }

    #[cfg(test)]
    pub(crate) fn any_running(&self) -> bool {
        self.tabs
            .iter()
            .any(|t| matches!(t.query, Query::Running { .. }))
    }

    #[cfg(test)]
    pub(crate) fn cancel_running(&self, tab_id: usize) {
        if let Some(Query::Running { cancel, .. }) =
            self.tabs.iter().find(|t| t.id == tab_id).map(|t| &t.query)
        {
            cancel.cancel();
        }
    }

    #[cfg(test)]
    pub(crate) fn clear_filter_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_browse_filter(window, cx);
    }

    #[cfg(test)]
    pub(crate) fn staged_changes(&self) -> usize {
        self.active_tab().edits.count()
    }

    #[cfg(test)]
    pub(crate) fn is_editing(&self) -> bool {
        self.active_tab().editing.is_some()
    }

    #[cfg(test)]
    pub(crate) fn cancel_edit_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_edit(window, cx);
    }

    /// Replace the open text area's contents.
    #[cfg(test)]
    pub(crate) fn type_into_area(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(area) = self
            .active_tab()
            .editing
            .as_ref()
            .and_then(|e| e.area.clone())
        {
            area.update(cx, |area, cx| area.set_value(text.to_string(), window, cx));
        }
    }

    /// The active tab's error, for tests.
    #[cfg(test)]
    pub(crate) fn error_text(&self) -> Option<String> {
        match &self.active_tab().query {
            Query::Failed(message) => Some(message.to_string()),
            _ => None,
        }
    }

    /// Why a query can't start on a tab now: a confirmation is waiting, or a
    /// query is already running on its connection (statements on one session
    /// must not interleave, or one's transaction swallows another's).
    /// A dialog, prompt or the palette is open: keys belong to it.
    pub(crate) fn modal_open(&self) -> bool {
        self.confirm.is_some()
            || self.connect_dialog.is_some()
            || self.settings.is_some()
            || self.palette.is_some()
            || self.parameters.is_some()
            || self.naming.is_some()
            || self.import.is_some()
            || self.switcher
    }

    pub(crate) fn busy(&self, tab_id: usize) -> Option<&'static str> {
        if self.confirm.is_some() {
            return Some("Answer the open prompt first.");
        }
        // Being checked (after the Mac slept) or reconnected: say so, rather
        // than quietly do nothing.
        let checking = self
            .tabs
            .iter()
            .find(|t| t.id == tab_id)
            .and_then(|t| t.session)
            .and_then(|id| self.session(id))
            .is_some_and(|s| s.reconnecting);
        if checking {
            return Some("Checking the connection: try again in a moment.");
        }
        self.connection_busy(tab_id).then_some(
            "A query or a save is already running on this connection: wait for it, or cancel it (⌘.).",
        )
    }

    /// A query or a save is running on `tab_id`'s connection. A save is a
    /// transaction: anything else run meanwhile would land inside it (and a
    /// `commit` would keep half of it).
    fn connection_busy(&self, tab_id: usize) -> bool {
        let Some(session) = self
            .tabs
            .iter()
            .find(|t| t.id == tab_id)
            .and_then(|t| t.session)
        else {
            return false;
        };
        self.tabs.iter().any(|t| {
            t.session == Some(session) && (matches!(t.query, Query::Running { .. }) || t.saving)
        })
    }

    /// The one way SQL from the editor reaches a connection: risky statements
    /// need a confirmation, and on production every write needs the
    /// connection's name typed (read-only connections are protected by the
    /// server). `row_limit` overrides Settings' (for Fetch all).
    fn guard(
        &mut self,
        tab_id: usize,
        sql: String,
        origin: Option<usize>,
        row_limit: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(why) = self.busy(tab_id) {
            self.flash(why, true, cx);
            return;
        }
        let Some(session) = self
            .tabs
            .iter()
            .find(|t| t.id == tab_id)
            .and_then(|t| t.session)
            .and_then(|id| self.session(id))
        else {
            return;
        };
        let production = session.environment == Environment::Production;
        // A transaction the script manages can't be kept read-only, so on
        // production its reads could write unseen (a function that writes):
        // ask, as for a write.
        let unguarded = production
            && assess(&sql).is_none()
            && xenon_driver::sql::opens_unguarded_transaction(&sql);
        let assessed = assess(&sql).or_else(|| unguarded.then(|| (Risk::Write, String::new())));
        if !session.read_only
            && let Some((risk, what)) = assessed
            && (production || (self.prefs.confirm_risky && risk >= Risk::EveryRow))
        {
            let body = match risk {
                _ if unguarded => "This starts or manages its own transaction, so what runs in \
                                   it isn't kept read-only by the server: a function that writes \
                                   would write."
                    .to_string(),
                Risk::Destructive => format!("This runs {what}, which can't be undone."),
                Risk::EveryRow => format!("This runs {what}, so it changes every row."),
                Risk::Write => format!("This {what} statement writes to the database."),
            };
            let body = if production {
                format!("{body} {} is a production connection.", session.name)
            } else {
                body
            };
            let body = if matches!(session.engine, Engine::MySql | Engine::MariaDb)
                && xenon_driver::sql::several_statements(&sql)
                && xenon_driver::sql::mysql_has_implicit_commit(&sql)
            {
                format!(
                    "{body} MySQL commits each schema change as it runs, so if a later statement \
                     fails, what ran before it stays."
                )
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
                    row_limit,
                },
                window,
                cx,
            );
            return;
        }
        let limit = row_limit.unwrap_or(self.prefs.row_limit);
        self.execute_with_limit(tab_id, sql, origin, limit, cx);
    }

    /// Run `sql` for a tab in the background, recording it in the history.
    pub(crate) fn execute(
        &mut self,
        tab_id: usize,
        sql: String,
        origin: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let limit = self.prefs.row_limit;
        self.execute_with_limit(tab_id, sql, origin, limit, cx);
    }

    /// Run again what the active tab last ran, keeping every row.
    /// Run what the active tab last ran again, keeping every row; it asks
    /// first like any run (a capped `delete … returning` is a write).
    pub(crate) fn fetch_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.active_tab();
        if let Some((sql, origin)) = tab.last_run.clone() {
            let id = tab.id;
            self.guard(id, sql, origin, Some(usize::MAX), window, cx);
        }
    }

    /// Run `sql` for a tab in the background, showing its first rows as soon as
    /// they arrive, and record it in the history.
    fn execute_with_limit(
        &mut self,
        tab_id: usize,
        sql: String,
        origin: Option<usize>,
        row_limit: usize,
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
        if self.session(session_id).is_some_and(|s| s.reconnecting) {
            return;
        }
        // Idle a while (the Mac may have slept): check it's still there first.
        if self.needs_check(session_id) {
            if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                tab.flash = Some(("Checking the connection…".into(), false));
            }
            cx.notify();
            self.check_then(
                session_id,
                tab_id,
                move |this, cx| this.execute_with_limit(tab_id, sql, origin, row_limit, cx),
                cx,
            );
            return;
        }
        if let Some(session) = self.session_mut(session_id) {
            session.last_used = std::time::Instant::now();
        }
        let Some(session) = self.session(session_id) else {
            return;
        };
        let conn = session.conn.clone();
        let store = self.store.clone().filter(|_| self.keeps_history(session));
        let cancel = conn.cancel_handle();
        // Production queries are stopped after the limit set in Settings → Safety.
        let limit = (session.environment == Environment::Production && self.prefs.query_limit > 0)
            .then(|| std::time::Duration::from_secs(self.prefs.query_limit));
        let limiter = cancel.clone();
        let began = std::time::Instant::now();
        // SQL Server cancels by ending the session, rolling back an open transaction.
        let keeps_session = !(session.engine == Engine::SqlServer && session.in_transaction);
        // On production, a script of several statements that writes runs as one
        // transaction: if one fails, none of it is kept.
        let production = session.environment == Environment::Production
            && !session.in_transaction
            && !xenon_driver::sql::controls_transaction(&sql);
        // MySQL commits a schema change as it runs: such a script can't be all or
        // nothing there, so it isn't wrapped as if it could (the prompt said so).
        let mysql = matches!(session.engine, Engine::MySql | Engine::MariaDb);
        let atomic = production
            && xenon_driver::sql::several_statements(&sql)
            && xenon_driver::sql::writes(&sql)
            && !(mysql && xenon_driver::sql::mysql_has_implicit_commit(&sql));
        // On production, what looks like a read runs in a transaction the server
        // keeps read-only, so a hidden write (a function that writes) is refused.
        let guarded = production && !xenon_driver::sql::writes(&sql);
        let wrap = if atomic {
            Some(Wrap::Atomic(conn.begin()))
        } else if guarded {
            conn.begin_read_only().map(Wrap::ReadOnly)
        } else {
            None
        };
        let live = Arc::new(Live::default());
        let feed = live.clone();
        let timed = live.clone();
        if let Some(limit) = limit.filter(|_| keeps_session) {
            watchdog(live.clone(), limiter.clone(), limit);
        }
        let last_sql = sql.clone();
        let work = cx.background_spawn(async move {
            let started = std::time::Instant::now();
            let begun = wrap.map(|wrap| conn.execute(wrap.begin(), 1));
            // A wrapped statement never runs unwrapped: if the transaction
            // couldn't start, nothing runs.
            let refused = match &begun {
                Some(Err(err)) => Some(anyhow::anyhow!(
                    "Couldn't start the transaction production statements run in, so nothing ran: {err:#}"
                )),
                _ => None,
            };
            let begun = matches!(begun, Some(Ok(_)));
            let result = if let Some(err) = refused {
                Err(err)
            } else {
                conn.execute_streaming(&sql, row_limit, &mut |progress| match progress {
                xenon_driver::Progress::Preview(rows) => {
                    *feed.preview.lock().unwrap_or_else(|e| e.into_inner()) = Some(rows);
                }
                xenon_driver::Progress::Rows(n) => {
                    feed.rows.store(n, std::sync::atomic::Ordering::Relaxed);
                }
                })
            };
            let result = match (wrap, begun) {
                (Some(wrap), true) => match result {
                    // Stopped part-way (⌘., the time limit, closing the tab): a
                    // script is all or nothing, so what ran is rolled back. A
                    // read keeps the rows it got.
                    Ok(done) if done.cancelled => {
                        let _ = conn.execute("rollback", 1);
                        match wrap {
                            Wrap::Atomic(_) => Err(wrap.explain(anyhow::anyhow!(
                                "Stopped before the end of the script"
                            ))),
                            Wrap::ReadOnly(_) => Ok(done),
                        }
                    }
                    Ok(done) => conn.execute("commit", 1).map(|_| done),
                    Err(err) => {
                        let _ = conn.execute("rollback", 1);
                        Err(wrap.explain(err))
                    }
                },
                _ => result,
            };
            feed.done.store(true, std::sync::atomic::Ordering::SeqCst);
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
            // When the server can't say, follow `begin` / `commit` in what ran.
            let in_transaction = conn.in_transaction().or_else(|| {
                result
                    .is_ok()
                    .then(|| xenon_driver::sql::transaction_effect(&sql))
                    .flatten()
            });
            (result, schemas, in_transaction, atomic && begun)
        });
        let task = cx.spawn(async move |this, cx| {
            let (result, schemas, in_transaction, atomic) = work.await;
            this.update(cx, |this, cx| {
                if let (Some(limit), true) = (
                    limit,
                    timed.timed_out.load(std::sync::atomic::Ordering::SeqCst),
                ) && let Some(tab) = this.tabs.iter_mut().find(|t| t.id == tab_id)
                {
                    tab.timed_out = Some(limit);
                }
                if let (Some(session), Some(open)) = (this.session_mut(session_id), in_transaction)
                {
                    session.in_transaction = open;
                }
                let note = match &result {
                    Ok(done) if atomic => Some(format!(
                        "Ran {} statements as one transaction.",
                        done.statements
                    )),
                    _ => None,
                };
                if let Some(session) = this.session_mut(session_id) {
                    session.last_used = std::time::Instant::now();
                }
                let lost = result.as_ref().is_err_and(crate::reconnect::may_be_lost);
                this.finish_query(tab_id, session_id, result, schemas, cx);
                if lost {
                    this.check_after_failure(session_id, tab_id, cx);
                }
                if let (Some(note), Some(tab)) =
                    (note, this.tabs.iter_mut().find(|t| t.id == tab_id))
                {
                    tab.flash = Some((note.into(), false));
                }
            })
            .ok();
        });
        // Show the first rows, and how many have arrived, while the rest load.
        self.next_run += 1;
        let run = self.next_run;
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(100))
                    .await;
                let done = live.done.load(std::sync::atomic::Ordering::SeqCst);
                let alive = this
                    .update(cx, |this, cx| {
                        let Some(tab) = this.tabs.iter_mut().find(|t| t.id == tab_id) else {
                            return;
                        };
                        if !matches!(tab.query, Query::Running { run: r, .. } if r == run) {
                            return;
                        }
                        if let Some(rows) = live
                            .preview
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .take()
                        {
                            let mut grid = Grid::new(rows);
                            if let Some(layout) = &tab.columns {
                                grid.apply_layout(layout);
                            }
                            tab.grid = Some(grid);
                            tab.selection = None;
                            tab.previewing = true;
                        }
                        tab.fetched = live.rows.load(std::sync::atomic::Ordering::Relaxed);
                        // (Where the limit can stop the query, a watchdog thread does.)
                        if let Some(limit) = limit
                            && !keeps_session
                            && tab.flash.is_none()
                            && began.elapsed() >= limit
                        {
                            {
                                tab.flash = Some((
                                    "Over the production time limit, but stopping it would end the \
                                     session and roll back the open transaction: cancel it yourself \
                                     (⌘.) if you mean to."
                                        .into(),
                                    true,
                                ));
                            }
                        }
                        cx.notify();
                    })
                    .is_ok();
                if done || !alive {
                    break;
                }
            }
        });
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) else {
            return;
        };
        tab.run_origin = origin;
        tab.error_hint = None;
        let editor = tab.editor.clone();
        editor.update(cx, |editor, cx| {
            if let Some(diagnostics) = editor.diagnostics_mut() {
                diagnostics.clear();
            }
            cx.notify();
        });
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) else {
            return;
        };
        tab.timed_out = None;
        tab.flash = None;
        tab.fetched = 0;
        tab.previewing = false;
        tab.sets.clear();
        tab.last_run = Some((last_sql, origin));
        tab.query = Query::Running {
            run,
            cancel,
            _task: task,
            _poll: poll,
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
            *session.catalog.borrow_mut() =
                Some(Rc::new(Catalog::new(schemas.clone(), session.conn.clone())));
            session.schemas = schemas;
        }
        self.reload_history(cx);
        // A table's remembered arrangement, the first time its rows arrive.
        let saved = self
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .filter(|tab| tab.columns.is_none())
            .and_then(|_| self.saved_column_layout(tab_id));
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) else {
            return;
        };
        if saved.is_some() {
            tab.columns = saved;
        }
        tab.selection = None;
        // New rows mean new row numbers: staged changes can't carry over.
        tab.edits = Rc::default();
        tab.editing = None;
        match result {
            Ok(mut result) => {
                // A script with several results: each set of rows gets a tab.
                tab.sets_total = result
                    .earlier
                    .iter()
                    .filter(|o| matches!(o, QueryOutcome::Rows(_)))
                    .count()
                    + usize::from(matches!(result.outcome, QueryOutcome::Rows(_)));
                let mut earlier: Vec<Grid> = std::mem::take(&mut result.earlier)
                    .into_iter()
                    .filter_map(|outcome| match outcome {
                        QueryOutcome::Rows(rows) => Some(Grid::new(rows)),
                        QueryOutcome::Affected(_) => None,
                    })
                    .take(MAX_RESULT_SETS - 1)
                    .collect();
                tab.sets.clear();
                if let QueryOutcome::Rows(rows) = &mut result.outcome {
                    let started = std::time::Instant::now();
                    let mut grid = Grid::new(std::mem::take(rows));
                    // Keep where the reader had scrolled in the preview.
                    if tab.previewing
                        && let Some(preview) = &tab.grid
                    {
                        grid.scroll = preview.scroll.clone();
                        grid.h_scroll = preview.h_scroll.clone();
                    }
                    if let Some(layout) = &tab.columns {
                        grid.apply_layout(layout);
                    }
                    tab.grid = Some(grid);
                    crate::trace("build results grid", started);
                    result.outcome = QueryOutcome::Rows(Default::default());
                    if !earlier.is_empty() {
                        tab.shown_set = earlier.len();
                        tab.sets = earlier.into_iter().map(Some).chain([None]).collect();
                    }
                } else {
                    // Ended with a statement that changes rows: show the last rows.
                    tab.grid = earlier.pop();
                    if !earlier.is_empty() {
                        tab.shown_set = earlier.len();
                        tab.sets = earlier.into_iter().map(Some).chain([None]).collect();
                    }
                }
                if tab.sets_total > tab.sets.len() && tab.sets.len() > 1 {
                    let left_out = tab.sets_total - tab.sets.len();
                    tab.flash = Some((
                        format!(
                            "{} results: showing the first {} and the last; {left_out} in between aren't shown.",
                            tab.sets_total,
                            tab.sets.len() - 1
                        )
                        .into(),
                        false,
                    ));
                }
                if result.cancelled && tab.timed_out.is_some() {
                    tab.flash = Some((time_limit_note(tab.timed_out).into(), true));
                }
                tab.query = Query::Done(Arc::new(result));
                if let Some(bench) = &mut self.bench
                    && matches!(bench.phase, crate::bench::Phase::Loading)
                {
                    bench.phase = crate::bench::Phase::Idle(crate::bench::Mark::now());
                    self.bench_schedule(cx);
                }
            }
            Err(err) if tab.timed_out.is_some() => {
                tab.grid = None;
                let mut note = time_limit_note(tab.timed_out);
                if err.chain().any(|e| e.to_string() == ATOMIC_ROLLED_BACK) {
                    note.push_str(
                        " The script ran as one transaction, so nothing it did was kept.",
                    );
                }
                tab.query = Query::Failed(note.into());
            }
            Err(err) => {
                tab.grid = None;
                let message = format!("{err:#}");
                tab.query = Query::Failed(message.clone().into());
                // Point at the mistake, and name a likely fix for a misspelt name.
                let position = err.downcast_ref::<QueryError>().and_then(|e| e.position);
                let unknown = crate::errors::unknown_name(&message);
                let suggestion = unknown.as_ref().and_then(|unknown| {
                    let schemas = &self.sessions.iter().find(|s| s.id == session_id)?.schemas;
                    crate::errors::did_you_mean(unknown, schemas)
                });
                let at = match (&tab.last_run, tab.run_origin) {
                    (Some((sql, _)), Some(origin)) => crate::errors::locate(
                        &tab.editor.read(cx).value(),
                        origin..origin + sql.len(),
                        position,
                        unknown.as_ref().map(|u| u.name.as_str()),
                    ),
                    _ => None,
                };
                if let Some(at) = &at {
                    self.pending_error_jump = Some((tab_id, at.clone()));
                }
                // Offer the fix only where the misspelt name really is (Postgres
                // points at `o` in `o.emial`, say).
                let name = unknown.as_ref().map(|u| u.name.clone());
                let text = tab.editor.read(cx).value().to_string();
                let points_at_name = matches!(
                    (&at, &name),
                    (Some(at), Some(name))
                        if text.get(at.clone()).is_some_and(|t| t.eq_ignore_ascii_case(name))
                );
                tab.error_hint = Some(ErrorHint {
                    at,
                    message: message.into(),
                    suggestion: suggestion.filter(|_| points_at_name).map(Into::into),
                    replaces: name.filter(|_| points_at_name),
                });
            }
        }
        self.find_total(tab_id, cx);
        cx.notify();
    }

    // ------------------------------------------------------------- bench

    fn bench_start(&mut self, cx: &mut Context<Self>) {
        let Some(bench) = &self.bench else { return };
        // A developer benchmark (XENON_BENCH): never on production.
        if self
            .active_session()
            .is_none_or(|s| s.environment == Environment::Production)
        {
            return;
        }
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

    /// Totals for the active tab's selected cells, worked out once per selection.
    pub(crate) fn selection_totals(&self) -> Option<crate::stats::Stats> {
        let tab = self.active_tab();
        let (grid, selection) = (tab.grid.as_ref()?, tab.selection?);
        let key = (
            Arc::as_ptr(&grid.data) as usize,
            Arc::as_ptr(&grid.order()) as usize,
            selection,
        );
        if let Some((cached, stats)) = &*self.selection_stats.borrow()
            && *cached == key
        {
            return stats.clone();
        }
        let rows = selection.rows().start..selection.rows().end.min(grid.row_count());
        let columns = selection.columns();
        let stats = crate::stats::of(rows.flat_map(|row| {
            columns
                .clone()
                .filter(|&c| c < grid.columns().len())
                .map(move |c| grid.cell(row, c))
        }));
        *self.selection_stats.borrow_mut() = Some((key, stats.clone()));
        stats
    }

    /// Put the suggested name in place of the misspelt one.
    pub(crate) fn fix_error(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = &mut self.tabs[self.active];
        let Some(hint) = &mut tab.error_hint else {
            return;
        };
        let (Some(at), Some(suggestion)) = (hint.at.clone(), hint.suggestion.take()) else {
            return;
        };
        hint.at = None;
        let editor = tab.editor.clone();
        // Typed since the error: the range may hold something else now.
        let still = hint.replaces.as_ref().is_some_and(|name| {
            editor
                .read(cx)
                .value()
                .get(at.clone())
                .is_some_and(|t| t.eq_ignore_ascii_case(name))
        });
        if !still {
            self.flash("The query has changed since: fix it by hand.", true, cx);
            return;
        }
        editor.update(cx, |editor, cx| {
            if let Some(diagnostics) = editor.diagnostics_mut() {
                diagnostics.clear();
            }
            editor.set_selected_range(at, cx);
            editor.replace(suggestion.to_string(), window, cx);
            editor.focus(window, cx);
        });
        cx.notify();
    }

    /// Put the editor's cursor on the word a failed statement points at.
    fn jump_to_error(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((tab_id, at)) = self.pending_error_jump.take() else {
            return;
        };
        if self.active_tab().id != tab_id {
            return;
        }
        let tab = self.active_tab();
        let message = tab
            .error_hint
            .as_ref()
            .map_or_else(Default::default, |hint| hint.message.clone());
        let editor = tab.editor.clone();
        editor.update(cx, |editor, cx| {
            let text = editor.value().to_string();
            if text.get(at.clone()).is_none() {
                return;
            }
            // Underline it, as an editor marks an error.
            let position = |offset: usize| {
                let before = &text[..offset];
                let line = before.matches('\n').count() as u32;
                let line_start = before.rfind('\n').map_or(0, |i| i + 1);
                let character = text[line_start..offset].encode_utf16().count() as u32;
                gpui_component::input::Position { line, character }
            };
            if let Some(diagnostics) = editor.diagnostics_mut() {
                diagnostics.clear();
                diagnostics.push(
                    gpui_base::input::Diagnostic::new(
                        position(at.start)..position(at.end),
                        message,
                    )
                    .with_severity(gpui_base::input::DiagnosticSeverity::Error),
                );
            }
            editor.set_selected_range(at, cx);
            editor.focus(window, cx);
        });
    }

    fn cancel_query(&mut self, _: &CancelQuery, window: &mut Window, cx: &mut Context<Self>) {
        // SQL Server cancels by ending the session, which rolls back an open
        // transaction too: say so first.
        if let Some(session) = self.active_session()
            && session.engine == Engine::SqlServer
            && session.in_transaction
            && matches!(self.active_tab().query, Query::Running { .. })
        {
            let body = "SQL Server stops a query by ending its session, so the open transaction \
                        is rolled back too: everything it changed is undone."
                .to_string();
            let tab_id = self.active_tab().id;
            let dialog = cx.new(|cx| {
                ConfirmDialog::new("Cancel and roll back?", body, "", None, window, cx)
                    .action("Cancel and roll back", true)
            });
            self.show_confirm(dialog, Pending::Cancel(tab_id), window, cx);
            return;
        }
        if let Query::Running { cancel, .. } = &self.active_tab().query {
            cancel.cancel();
        }
    }

    /// Names of connections with a transaction open.
    fn open_transactions(&self) -> Vec<String> {
        self.sessions
            .iter()
            .filter(|s| s.in_transaction)
            .map(|s| s.name.clone())
            .collect()
    }

    /// Quit, asking first when a transaction is open (quitting rolls it back).
    /// Returns whether Xenon may close now.
    pub(crate) fn may_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let open = self.open_transactions();
        let unsaved: usize = self.tabs.iter().map(|t| t.edits.count()).sum();
        if open.is_empty() && unsaved == 0 {
            return true;
        }
        let (title, body, action) = if open.is_empty() {
            (
                "Discard changes?",
                format!("{} not saved. Quitting discards them.", changes(unsaved)),
                "Discard and quit",
            )
        } else {
            let also = if unsaved > 0 {
                format!(" {} not saved are discarded too.", changes(unsaved))
            } else {
                String::new()
            };
            (
                "Roll back and quit?",
                format!(
                    "A transaction is open on {}. Quitting rolls back everything it changed.{also}",
                    open.join(", ")
                ),
                "Roll back and quit",
            )
        };
        let dialog =
            cx.new(|cx| ConfirmDialog::new(title, body, "", None, window, cx).action(action, true));
        self.show_confirm(dialog, Pending::Quit, window, cx);
        false
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
        self.browse_where(session, schema, table, None, window, cx);
    }

    /// Open a table's tab (or switch to it), with `filter` in its WHERE box
    /// when given, and load its rows.
    pub(crate) fn browse_where(
        &mut self,
        session: usize,
        schema: &str,
        table: &str,
        filter: Option<String>,
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
        // Its tab has changes staged: show it as it is, rather than change its
        // filter and then refuse to reload it.
        if filter.is_some() && self.guard_edits(cx) {
            return;
        }
        let mut needs_info = false;
        if let Some(browse) = &mut self.tabs[self.active].browse {
            browse.structure = false;
            needs_info = browse.info.is_none();
            if let Some(filter) = filter {
                browse.order = None;
                let input = browse.filter.clone();
                input.update(cx, |input, cx| input.set_value(filter, window, cx));
            }
        }
        if needs_info {
            self.load_table_info(cx);
        }
        self.run_browse(window, cx);
    }

    /// Fetch the active table tab's columns, keys and indexes in the background.
    pub(crate) fn load_table_info(&mut self, cx: &mut Context<Self>) {
        let tab = self.active_tab();
        let (Some(session), Some(browse)) = (self.active_session(), &tab.browse) else {
            return;
        };
        let (conn, tab_id) = (session.conn.clone(), tab.id);
        let (schema, table) = (browse.schema.clone(), browse.table.clone());
        let task = cx.background_spawn(async move { conn.describe(&schema, &table) });
        cx.spawn(async move |this, cx| {
            let info = task.await;
            this.update(cx, |this, cx| {
                let Some(browse) = this
                    .tabs
                    .iter_mut()
                    .find(|t| t.id == tab_id)
                    .and_then(|t| t.browse.as_mut())
                else {
                    return;
                };
                match info {
                    Ok(info) => {
                        browse.info = Some(Rc::new(info));
                        browse.info_error = None;
                    }
                    Err(err) => browse.info_error = Some(format!("{err:#}").into()),
                }
                // Its size decides whether to count its rows.
                this.find_total(tab_id, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Build (or rebuild) the Structure tab's highlighted definition: an editor
    /// needs a window, so it's made here rather than while drawing.
    fn refresh_definition(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = &self.tabs[self.active];
        let (Some(browse), Some(session)) =
            (&tab.browse, tab.session.and_then(|id| self.session(id)))
        else {
            return;
        };
        let Some(info) = browse.info.as_ref().filter(|_| browse.structure) else {
            return;
        };
        let is_view = session
            .schemas
            .iter()
            .find(|s| s.name == browse.schema)
            .and_then(|s| s.objects.iter().find(|o| o.name == browse.table))
            .is_some_and(|o| o.kind == ObjectKind::View);
        let conn = session.conn.clone();
        let (text, _) = crate::structure::definition(&crate::structure::View {
            info,
            table: &browse.table,
            is_view,
            ident: &|name| conn.ident(name),
            open: Rc::new(|_, _, _, _| {}),
            editor: None,
        });
        if browse
            .definition
            .as_ref()
            .is_some_and(|(shown, _)| *shown == text)
        {
            return;
        }
        let editor = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .language("sql")
                .line_number(false)
                .soft_wrap(false)
                .searchable(true)
                .default_value(text.clone());
            state.set_readonly(true, cx);
            state
        });
        if let Some(browse) = &mut self.tabs[self.active].browse {
            browse.definition = Some((text, editor));
        }
    }

    /// Switch the active table tab between its rows and its structure.
    pub(crate) fn toggle_structure(&mut self, cx: &mut Context<Self>) {
        let Some(browse) = &mut self.tabs[self.active].browse else {
            return;
        };
        browse.structure = !browse.structure;
        if browse.structure && browse.info.is_none() {
            self.load_table_info(cx);
        }
        cx.notify();
    }

    /// Open the row a foreign-key cell points at, in its table's tab.
    pub(crate) fn follow_key(
        &mut self,
        row: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = self.active_tab();
        let (Some(session), Some(browse), Some(grid)) =
            (self.active_session(), &tab.browse, &tab.grid)
        else {
            return;
        };
        let Some(name) = grid.data.columns.get(column).map(|c| c.name.as_str()) else {
            return;
        };
        let Some(key) = browse.key_for(name) else {
            return;
        };
        let Some(value) = (row < grid.row_count()).then(|| &grid.row(row)[column]) else {
            return;
        };
        if value.is_null() {
            return;
        }
        let (_, to) = key.single().expect("key_for finds single-column keys");
        let filter = format!(
            "{} = {}",
            session.conn.ident(to),
            session.conn.literal(value)
        );
        let (id, schema, table) = (session.id, key.ref_schema.clone(), key.ref_table.clone());
        self.browse_where(id, &schema, &table, Some(filter), window, cx);
    }

    /// Open a table from the Structure view.
    fn open_table(
        &mut self,
        schema: String,
        table: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.active_tab().session {
            self.browse(id, &schema, &table, window, cx);
        }
    }

    /// Regenerate the active browse tab's query from its filter and sort, and run it.
    pub(crate) fn run_browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.guard_edits(cx) {
            return;
        }
        let (Some(session), Some(browse)) = (self.active_session(), &self.active_tab().browse)
        else {
            return;
        };
        let conn = session.conn.clone();
        let filter = browse.filter.read(cx).value().to_string();
        // A new filter or sort starts again at the first page, and is counted afresh.
        let paged_for = Some((filter.trim().to_string(), browse.order.clone()));
        let restart = browse.paged_for != paged_for;
        let page = if restart { 0 } else { browse.page };
        // Named in full, so a `USE` or `search_path` change in some tab can't
        // point it at another table of the same name.
        let name = xenon_driver::qualified_name(conn.as_ref(), Some(&browse.schema), &browse.table);
        let sql = xenon_driver::sql::browse_page(
            |ident| conn.ident(ident),
            &name,
            &filter,
            browse
                .order
                .as_ref()
                .map(|(column, dir)| (column.as_str(), *dir)),
            self.prefs.browse_limit,
            page * self.prefs.browse_limit,
            conn.uses_top(),
        );
        let editor = self.active_tab().editor.clone();
        // The filter goes into a `where`: it can narrow the rows, not change them.
        if xenon_driver::sql::writes(&sql) {
            self.flash(
                "The filter can only narrow the rows: it can't run other statements.",
                true,
                cx,
            );
            return;
        }
        editor.update(cx, |editor, cx| editor.set_value(sql.clone(), window, cx));
        self.tabs[self.active].save_unsure = false;
        if let Some(browse) = &mut self.tabs[self.active].browse {
            browse.applied = filter;
            browse.loaded_with = Some(sql);
            browse.page = page;
            if restart {
                browse.paged_for = paged_for;
                browse.total = None;
            }
        }
        self.run(RunScope::All, window, cx);
    }

    /// Running a table tab whose WHERE field was typed in but not applied: apply it,
    /// rather than re-run the old query.
    fn apply_pending_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let pending =
            self.active_tab().browse.as_ref().is_some_and(|browse| {
                browse.filter.read(cx).value().trim() != browse.applied.trim()
            });
        if pending {
            self.run_browse(window, cx);
        }
        pending
    }

    fn clear_browse_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.guard_edits(cx) {
            return;
        }
        if let Some(browse) = &mut self.tabs[self.active].browse {
            browse.order = None;
            let filter = browse.filter.clone();
            filter.update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.run_browse(window, cx);
    }

    // ------------------------------------------------------------- editing

    /// The table the active tab's rows can be saved to, or why they can't be.
    pub(crate) fn edit_target(&self) -> Result<TableRef, &'static str> {
        let tab = self.active_tab();
        let Some(browse) = &tab.browse else {
            return Err("Query results are read-only; open a table from the explorer to edit it.");
        };
        let Some(session) = self.active_session() else {
            return Err("Not connected.");
        };
        if session.read_only {
            return Err("Read-only connection.");
        }
        let Some(grid) = &tab.grid else {
            return Err("Nothing loaded yet.");
        };
        if matches!(tab.query, Query::Running { .. }) {
            return Err("Still loading.");
        }
        // Changes made now would be thrown away when the save finishes.
        if tab.saving {
            return Err("Saving: wait for it to finish.");
        }
        let ran = tab.last_run.as_ref().map(|(sql, _)| sql.trim());
        if ran.is_none() || ran != browse.loaded_with.as_deref().map(str::trim) {
            return Err(
                "These rows come from other SQL, not this table: reopen the table to edit it.",
            );
        }
        let object = session
            .schemas
            .iter()
            .find(|s| s.name == browse.schema)
            .and_then(|s| s.objects.iter().find(|o| o.name == browse.table));
        let Some(object) = object else {
            return Err("This table is no longer in the schema.");
        };
        if object.kind == ObjectKind::View {
            return Err("Views are read-only.");
        }
        // Each key column must be in the result to find its rows again.
        let key: Option<Vec<usize>> = object
            .columns
            .iter()
            .filter(|c| c.primary_key)
            .map(|c| grid.data.columns.iter().position(|r| r.name == c.name))
            .collect();
        match key {
            Some(key) if !key.is_empty() => Ok(TableRef {
                schema: Some(browse.schema.clone()),
                table: browse.table.clone(),
                key,
            }),
            _ => Err("No primary key, so rows can't be told apart: read-only."),
        }
    }

    /// Rows shown in a tab: those fetched, then those being added.
    fn total_rows(tab: &Tab) -> usize {
        tab.grid.as_ref().map_or(0, |g| g.row_count()) + tab.edits.inserted.len()
    }

    fn row_ref(tab: &Tab, ix: usize) -> Option<RowRef> {
        let grid = tab.grid.as_ref()?;
        if ix < grid.row_count() {
            Some(RowRef::Existing(grid.data_index(ix)))
        } else {
            let new = ix - grid.row_count();
            (new < tab.edits.inserted.len()).then_some(RowRef::New(new))
        }
    }

    pub(crate) fn flash(
        &mut self,
        text: impl Into<SharedString>,
        error: bool,
        cx: &mut Context<Self>,
    ) {
        self.tabs[self.active].flash = Some((text.into(), error));
        cx.notify();
    }

    /// Changes are pending: say so instead of throwing them away.
    pub(crate) fn guard_edits(&mut self, cx: &mut Context<Self>) -> bool {
        if self.active_tab().edits.is_empty() {
            return false;
        }
        self.flash("Save or discard your changes first.", true, cx);
        true
    }

    /// Open the edit bar on a cell (display row `ix`).
    pub(crate) fn begin_edit(
        &mut self,
        ix: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Why it can't be edited already shows in the results bar.
        if self.edit_target().is_err() {
            return;
        }
        let tab = &self.tabs[self.active];
        let (Some(row), Some(grid)) = (Self::row_ref(tab, ix), tab.grid.as_ref()) else {
            return;
        };
        let current: Option<Value> = match tab.edits.value(row, column) {
            Some(value) => value.cloned(),
            None => match row {
                RowRef::Existing(r) => Some(grid.data.rows[r][column].clone()),
                RowRef::New(_) => None,
            },
        };
        if matches!(current, Some(Value::Blob(_))) {
            self.flash("Binary values can't be edited here yet.", false, cx);
            return;
        }
        let (text, placeholder) = match &current {
            None => (String::new(), "default"),
            Some(Value::Null) => (String::new(), "NULL"),
            Some(value) => (value.display().into_owned(), ""),
        };
        // The column's type as the table declares it (with an enum's values),
        // else as the result reports it.
        let column_info = tab
            .browse
            .as_ref()
            .and_then(|b| b.info.as_ref())
            .and_then(|info| {
                info.columns
                    .iter()
                    .find(|c| c.name == grid.data.columns[column].name)
            });
        let type_name = column_info
            .map(|c| c.type_name.clone())
            .or_else(|| grid.data.columns[column].type_name.clone());
        let options = column_info.map(|c| c.options.clone()).unwrap_or_default();
        let kind = crate::value_editor::kind_of(type_name.as_deref(), &options, current.as_ref());
        let area = matches!(
            kind,
            crate::value_editor::ValueKind::Text | crate::value_editor::ValueKind::Json
        )
        .then(|| {
            let json = kind == crate::value_editor::ValueKind::Json;
            let shown = if json {
                crate::value_editor::pretty(&text)
            } else {
                text.clone()
            };
            cx.new(|cx| {
                let state = EditorState::new(window, cx)
                    .soft_wrap(true)
                    .line_number(false)
                    .placeholder(placeholder)
                    .default_value(shown);
                if json { state.language("json") } else { state }
            })
        });
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
                .default_value(text)
        });
        match &area {
            Some(area) => area.update(cx, |area, cx| area.focus(window, cx)),
            None => input.update(cx, |input, cx| input.focus(window, cx)),
        }
        let enter = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.commit_edit(false, window, cx);
            }
        });
        let tab = &mut self.tabs[self.active];
        let shown_at = tab
            .grid
            .as_ref()
            .and_then(|g| g.display_of(column))
            .unwrap_or(0);
        tab.selection = Some(Selection::cell(ix, shown_at));
        tab.flash = None;
        // Values too big for a cell (long text, JSON, a choice) are edited in
        // the inspector: show it.
        let in_place = matches!(
            kind,
            crate::value_editor::ValueKind::Line | crate::value_editor::ValueKind::Date(_)
        );
        tab.editing = Some(CellEditor {
            row,
            column,
            kind,
            input,
            area,
            _enter: enter,
        });
        if !in_place {
            self.set_inspector(true);
        }
        cx.notify();
    }

    /// Stage the edited value (or NULL) and finish editing.
    pub(crate) fn commit_edit(
        &mut self,
        set_null: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = &mut self.tabs[self.active];
        let Some(open) = tab.editing.as_ref() else {
            return;
        };
        let text = match &open.area {
            Some(area) => area.read(cx).value().to_string(),
            None => open.input.read(cx).value().to_string(),
        };
        // JSON is checked before it's staged; the editor stays open to fix it.
        if !set_null
            && open.kind == crate::value_editor::ValueKind::Json
            && !text.trim().is_empty()
            && let Some(problem) = crate::value_editor::json_error(&text)
        {
            tab.flash = Some((problem.into(), true));
            cx.notify();
            return;
        }
        let Some(editor) = tab.editing.take() else {
            return;
        };
        let Some(grid) = &tab.grid else { return };
        let original = match editor.row {
            RowRef::Existing(r) => Some(grid.data.rows[r][editor.column].clone()),
            RowRef::New(_) => None,
        };
        let numeric = original.as_ref().is_some_and(Value::is_numeric)
            || grid.data.columns[editor.column]
                .type_name
                .as_deref()
                .is_some_and(crate::value_editor::is_numeric_type);
        // Laid out for editing, JSON reads differently but may mean the same.
        let same_json = editor.kind == crate::value_editor::ValueKind::Json
            && original
                .as_ref()
                .is_some_and(|o| crate::value_editor::same_json(&o.display(), &text));
        let value = if set_null {
            Value::Null
        } else if same_json
            || original
                .as_ref()
                .is_some_and(|o| !o.is_null() && o.display() == text)
            // A NULL cell opens empty: left empty, it's still NULL, not ''.
            || (text.is_empty() && original.as_ref().is_some_and(Value::is_null))
        {
            original.clone().expect("checked above")
        } else if numeric && crate::value_editor::is_plain_number(text.trim()) {
            Value::Decimal(text.trim().to_string())
        } else {
            Value::Text(text)
        };
        Rc::make_mut(&mut tab.edits).set(editor.row, editor.column, value, original.as_ref());
        self.grid_focus.focus(window, cx);
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn last_row(&self) -> usize {
        Self::total_rows(self.active_tab()) - 1
    }

    /// Type into the open edit bar (snapshots drive the UI without a keyboard).
    #[cfg(test)]
    pub(crate) fn type_into_cell(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = &self.tabs[self.active].editing {
            let input = editor.input.clone();
            input.update(cx, |input, cx| {
                input.set_value(text.to_string(), window, cx)
            });
        }
    }

    fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs[self.active].editing = None;
        self.grid_focus.focus(window, cx);
        cx.notify();
    }

    fn edit_cell(&mut self, _: &EditCell, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.active_tab();
        if let (Some(selection), Some(grid)) = (tab.selection, &tab.grid) {
            let (row, shown) = selection.head;
            if let Some(column) = grid.column(shown) {
                self.begin_edit(row, column, window, cx);
            }
        }
    }

    /// Mark the selected rows for deletion (or, if they all are, unmark them);
    /// rows being added are simply removed.
    pub(crate) fn delete_rows(&mut self, _: &DeleteRows, _: &mut Window, cx: &mut Context<Self>) {
        // Typed inside a dialog, it's not for the tab behind it.
        if self.modal_open() {
            return;
        }
        if let Err(why) = self.edit_target() {
            self.flash(why, false, cx);
            return;
        }
        let tab = &mut self.tabs[self.active];
        let Some(selection) = tab.selection else {
            return;
        };
        let rows: Vec<RowRef> = selection
            .rows()
            .filter_map(|ix| Self::row_ref(tab, ix))
            .collect();
        let edits = Rc::make_mut(&mut tab.edits);
        let existing: Vec<usize> = rows
            .iter()
            .filter_map(|r| match r {
                RowRef::Existing(r) => Some(*r),
                RowRef::New(_) => None,
            })
            .collect();
        let all_marked = !existing.is_empty() && existing.iter().all(|r| edits.deleted.contains(r));
        for row in existing {
            if all_marked {
                edits.deleted.remove(&row);
            } else {
                edits.deleted.insert(row);
            }
        }
        let mut new: Vec<usize> = rows
            .iter()
            .filter_map(|r| match r {
                RowRef::New(n) => Some(*n),
                RowRef::Existing(_) => None,
            })
            .collect();
        new.sort_unstable_by(|a, b| b.cmp(a));
        // An open edit on a new row would land on another once the rows after a
        // removed one move up: close it.
        if let Some(open) = &tab.editing
            && let RowRef::New(k) = open.row
            && new.iter().any(|&n| n <= k)
        {
            tab.editing = None;
        }
        for n in new {
            edits.inserted.remove(n);
        }
        tab.flash = None;
        cx.notify();
    }

    pub(crate) fn add_row(&mut self, _: &AddRow, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(why) = self.edit_target() {
            self.flash(why, false, cx);
            return;
        }
        let tab = &mut self.tabs[self.active];
        let Some(grid) = &tab.grid else { return };
        let columns = grid.data.columns.len();
        Rc::make_mut(&mut tab.edits)
            .inserted
            .push(vec![None; columns]);
        let last = Self::total_rows(tab) - 1;
        grid.scroll.scroll_to_item(last, ScrollStrategy::Nearest);
        tab.selection = Some(Selection::cell(last, 0));
        tab.flash = None;
        self.grid_focus.focus(window, cx);
        cx.notify();
    }

    pub(crate) fn discard_edits(
        &mut self,
        _: &DiscardEdits,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = &mut self.tabs[self.active];
        tab.edits = Rc::default();
        tab.editing = None;
        tab.flash = None;
        cx.notify();
    }

    /// The statements that would save the active tab's changes.
    fn pending_statements(&self) -> Result<Vec<edits::Statement>, &'static str> {
        let target = self.edit_target()?;
        let tab = self.active_tab();
        let (Some(session), Some(grid)) = (self.active_session(), &tab.grid) else {
            return Err("Nothing loaded yet.");
        };
        if edits::touches_null_key(&target, &grid.data, &tab.edits) {
            return Err(
                "A changed or deleted row has no key (it's NULL), so it can't be told apart from \
                 others: undo that change to save the rest.",
            );
        }
        Ok(edits::statements(
            session.conn.as_ref(),
            &target,
            &grid.data,
            &tab.edits,
        ))
    }

    fn review_edits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_edits_confirming(true, window, cx);
    }

    /// ⌘S: save the active tab's staged changes, or else its query.
    pub(crate) fn save_edits(
        &mut self,
        _: &SaveEdits,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Typed inside a dialog, it's not for the tab behind it.
        if self.modal_open() {
            return;
        }
        let tab = self.active_tab();
        if tab.edits.is_empty() && tab.editing.is_none() {
            self.save_query(false, window, cx);
            return;
        }
        self.save_edits_confirming(false, window, cx);
    }

    /// Save the active tab's changes. Production always asks (with its name
    /// typed); so do deletions when "ask before risky statements" is on, and
    /// any save the user asked to review.
    fn save_edits_confirming(&mut self, review: bool, window: &mut Window, cx: &mut Context<Self>) {
        // Saving begins and ends its own transaction, which would commit (or
        // roll back) the one the user has open.
        if self.active_session().is_some_and(|s| s.in_transaction) {
            self.flash(
                "A transaction is open: commit or roll it back (status bar) before saving changes.",
                true,
                cx,
            );
            return;
        }
        // ⌘S while the edit bar is open keeps what was typed.
        if self.active_tab().editing.is_some() {
            self.commit_edit(false, window, cx);
        }
        if self.active_tab().edits.is_empty() || self.active_tab().saving {
            return;
        }
        if self.active_tab().save_unsure {
            self.flash(
                "The last save may or may not have been kept: reload the table to see what's there before saving again.",
                true,
                cx,
            );
            return;
        }
        if let Some(why) = self.busy(self.active_tab().id) {
            self.flash(why, true, cx);
            return;
        }
        let statements = match self.pending_statements() {
            Ok(statements) => statements,
            Err(why) => {
                self.flash(why, true, cx);
                return;
            }
        };
        let tab_id = self.active_tab().id;
        let Some(session) = self.active_session() else {
            return;
        };
        let production = session.environment == Environment::Production;
        let deletes = !self.active_tab().edits.deleted.is_empty();
        if production || review || (deletes && self.prefs.confirm_risky) {
            let count = self.active_tab().edits.count();
            let rows = if count == 1 {
                "1 row".to_string()
            } else {
                format!("{count} rows")
            };
            let (title, body) = if production {
                (
                    "Save to production?".to_string(),
                    format!(
                        "This changes {rows} in {}, a production connection. It all saves or none of it does.",
                        session.name
                    ),
                )
            } else {
                (
                    "Save changes?".to_string(),
                    format!(
                        "This changes {rows}, in one transaction: it all saves or none of it does."
                    ),
                )
            };
            let require = production.then(|| session.name.clone());
            let script = edits::script(session.conn.as_ref(), &statements);
            let dialog = cx.new(|cx| {
                ConfirmDialog::new(title, body, &script, require, window, cx)
                    .action("Save changes", false)
            });
            self.show_confirm(dialog, Pending::Save { tab_id, statements }, window, cx);
            return;
        }
        self.save_statements(tab_id, statements, window, cx);
    }

    /// Run the save in the background, then reload the table.
    fn save_statements(
        &mut self,
        tab_id: usize,
        statements: Vec<edits::Statement>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Confirmed while something else started on the connection: not now.
        if self.connection_busy(tab_id) {
            self.flash(
                "A query or a save is already running on this connection: save again when it's done.",
                true,
                cx,
            );
            return;
        }
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) else {
            return;
        };
        let Some(session) = tab
            .session
            .and_then(|id| self.sessions.iter().find(|s| s.id == id))
        else {
            return;
        };
        let conn = session.conn.clone();
        // As `keeps_history`, by field: `tab` holds `self.tabs` here.
        let keep = session.environment != Environment::Production || self.prefs.production_history;
        let store = self.store.clone().filter(|_| keep);
        // Production saves stop at the time limit, like queries.
        let limit = (session.environment == Environment::Production && self.prefs.query_limit > 0)
            .then(|| std::time::Duration::from_secs(self.prefs.query_limit));
        tab.saving = true;
        tab.flash = None;
        cx.notify();
        let work = cx.background_spawn(async move {
            let started = std::time::Instant::now();
            let saved = edits::save_within(conn.as_ref(), &statements, limit);
            if let Some(store) = store {
                let script = edits::script(conn.as_ref(), &statements);
                let (rows, error) = match &saved {
                    Ok(rows) => (Some(*rows as u64), None),
                    Err(err) => (None, Some(format!("{err:#}"))),
                };
                let _ = store.record(NewHistoryEntry {
                    connection: &conn.id(),
                    sql: &script,
                    duration: started.elapsed(),
                    rows,
                    error: error.as_deref(),
                });
            }
            saved
        });
        cx.spawn_in(window, async move |this, cx| {
            let saved = work.await;
            this.update_in(cx, |this, window, cx| {
                let Some(index) = this.tabs.iter().position(|t| t.id == tab_id) else {
                    return;
                };
                let tab = &mut this.tabs[index];
                tab.saving = false;
                match saved {
                    Ok(rows) => {
                        tab.edits = Rc::default();
                        let message = format!(
                            "Saved · {rows} {} changed",
                            if rows == 1 { "row" } else { "rows" }
                        );
                        if index == this.active {
                            this.run_browse(window, cx);
                        }
                        this.tabs[index].flash = Some((message.into(), false));
                    }
                    Err(err) => {
                        tab.save_unsure = err.downcast_ref::<xenon_edit::CommitUnknown>().is_some();
                        tab.flash = Some((format!("{err:#}").into(), true));
                    }
                }
                this.reload_history(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---------------------------------------------------------------- grid

    /// Header click: browse tabs sort on the server (the whole table, not just
    /// the rows fetched); query tabs sort the rows they already have.
    fn sort_column(&mut self, column: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.guard_edits(cx) {
            return;
        }
        // Sorting a big table by a column no index leads with reads all of it: ask.
        let tab = &self.tabs[self.active];
        if let (Some(browse), Some(grid)) = (&tab.browse, &tab.grid)
            && let Some(info) = &browse.info
            && let Some(rows) = info.estimated_rows.filter(|n| *n >= LARGE_TABLE)
            && let Some(name) = grid.data.columns.get(column).map(|c| c.name.clone())
            && !info.indexed(&name)
            && browse.order.as_ref().is_none_or(|(c, _)| *c != name)
        {
            let body = format!(
                "{} has about {} rows and no index on {name}, so the server reads all of them \
                 to sort. On a busy server, that can slow everyone down.",
                browse.table,
                group_digits(rows as usize)
            );
            let tab_id = tab.id;
            let dialog = cx.new(|cx| {
                ConfirmDialog::new("Sort the whole table?", body, "", None, window, cx)
                    .action("Sort anyway", false)
            });
            self.show_confirm(dialog, Pending::Sort { tab_id, column }, window, cx);
            return;
        }
        self.sort_now(column, window, cx);
    }

    fn sort_now(&mut self, column: usize, window: &mut Window, cx: &mut Context<Self>) {
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
        // Clicking away from a cell being typed into keeps what was typed.
        if self
            .inline_edit()
            .is_some_and(|(r, c, _)| (r, c) != (row, column))
        {
            self.commit_edit(false, window, cx);
        }
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
        let (rows, columns) = (
            grid.row_count() + tab.edits.inserted.len(),
            grid.columns().len(),
        );
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
                head: (grid.row_count() - 1, grid.columns().len() - 1),
            });
            cx.notify();
        }
    }

    fn copy(&mut self, format: Format, cx: &mut Context<Self>) {
        let tab = self.active_tab();
        let (Some(grid), Some(selection)) = (&tab.grid, tab.selection) else {
            return;
        };
        // Selections are in display order: copy what's shown, as it's shown.
        let shown: Vec<usize> = selection.columns().filter_map(|c| grid.column(c)).collect();
        let columns: Vec<_> = shown.iter().map(|&c| &grid.data.columns[c]).collect();
        let rows: Vec<Vec<_>> = selection
            .rows()
            .map(|r| shown.iter().map(|&c| &grid.row(r)[c]).collect())
            .collect();
        let target = self.sql_target();
        let ident = |name: &str| {
            target
                .as_ref()
                .map_or(name.to_string(), |(_, c)| c.ident(name))
        };
        let literal = |value: &xenon_driver::Value| {
            target
                .as_ref()
                .map_or_else(|| value.display().into_owned(), |(_, c)| c.literal(value))
        };
        let sql = target.as_ref().map(|(table, _)| export::Sql {
            table: table.clone(),
            ident: &ident,
            literal: &literal,
        });
        cx.write_to_clipboard(ClipboardItem::new_string(export::render_sql(
            format,
            &columns,
            &rows,
            sql.as_ref(),
        )));
    }

    /// The table SQL inserts from the active tab go into, quoted for its
    /// connection, and that connection. Query results go into `results`.
    fn sql_target(&self) -> Option<(String, Arc<dyn xenon_driver::Connection>)> {
        let conn = self.active_session()?.conn.clone();
        let table = match &self.active_tab().browse {
            Some(browse) => {
                xenon_driver::qualified_name(conn.as_ref(), Some(&browse.schema), &browse.table)
            }
            None => "results".to_string(),
        };
        Some((table, conn))
    }

    /// Save the active tab's whole result, in the order shown, to a file the user picks.
    fn export(&mut self, format: crate::export::FileFormat, cx: &mut Context<Self>) {
        self.export_menu = false;
        let tab = self.active_tab();
        let Some(grid) = &tab.grid else { return };
        let tab_id = tab.id;
        let name = tab
            .browse
            .as_ref()
            .map_or("results".to_string(), |browse| browse.table.clone());
        let job = crate::export::Job {
            data: grid.data.clone(),
            order: grid.order(),
            sql: self.sql_target(),
        };
        let truncated = grid.data.truncated;
        let dir = self
            .export_dir
            .clone()
            .or_else(dirs::download_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| "/".into());
        let picked = cx.prompt_for_new_path(&dir, Some(&format!("{name}.{}", format.extension())));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = picked.await else {
                return;
            };
            let target = path.clone();
            let saved = cx
                .background_spawn(async move { crate::export::save(&job, format, &target) })
                .await;
            this.update(cx, |this, cx| {
                this.export_dir = path.parent().map(Into::into);
                let Some(tab) = this.tabs.iter_mut().find(|t| t.id == tab_id) else {
                    return;
                };
                let file = path
                    .file_name()
                    .map_or_else(Default::default, |f| f.to_string_lossy());
                tab.flash = Some(match saved {
                    Ok(rows) => {
                        let noun = if rows == 1 { "row" } else { "rows" };
                        let first = if truncated { "the first " } else { "" };
                        (
                            format!("Exported {first}{} {noun} to {file}", group_digits(rows))
                                .into(),
                            false,
                        )
                    }
                    Err(err) => (format!("{err:#}").into(), true),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
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
            Drag::Inspector {
                start_x,
                start_width,
            } => {
                // Its left edge: dragging left widens it, as long as the results
                // keep room to work in.
                let beside = RAIL_WIDTH
                    + if self.show_side_panel {
                        self.side_panel_width
                    } else {
                        0.
                    };
                let room = f32::from(window.viewport_size().width) - beside - MIN_RESULTS_WIDTH;
                let max = MAX_INSPECTOR_WIDTH.min(room).max(MIN_INSPECTOR_WIDTH);
                self.inspector_width = (start_width - f32::from(event.position.x - *start_x))
                    .clamp(MIN_INSPECTOR_WIDTH, max)
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
        let commands: [(&str, &str, Box<dyn gpui::Action>); 35] = [
            ("Run Statement", "⌘↵", Box::new(RunQuery)),
            ("Run All", "⇧⌘↵", Box::new(RunAll)),
            ("Explain", "⌘E", Box::new(Explain)),
            ("Cancel Query", "⌘.", Box::new(CancelQuery)),
            ("New Query Tab", "⌘T", Box::new(NewTab)),
            ("Close Tab", "⌘W", Box::new(CloseTab)),
            ("Next Tab", "⌃⇥", Box::new(NextTab)),
            ("Previous Tab", "⌃⇧⇥", Box::new(PrevTab)),
            ("Settings…", "⌘,", Box::new(OpenSettings)),
            ("Save Changes", "⌘S", Box::new(SaveEdits)),
            ("Discard Changes", "", Box::new(DiscardEdits)),
            ("Add Row", "", Box::new(AddRow)),
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
            ("Copy Cells as Markdown", "", Box::new(CopyMarkdown)),
            ("Copy Cells as SQL Inserts", "", Box::new(CopySql)),
            ("Export Results…", "⇧⌘S", Box::new(ExportResults)),
            ("Show Table Structure", "", Box::new(ToggleStructure)),
            ("Save Query", "", Box::new(SaveQuery)),
            ("Save Changes", "⌘S", Box::new(SaveEdits)),
            ("Save Query As…", "", Box::new(SaveQueryAs)),
            ("Show Saved Queries", "", Box::new(ShowSaved)),
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
        let prefs = Prefs {
            theme: choice,
            ..self.prefs
        };
        self.set_prefs(prefs, window, cx);
    }

    /// Step the text size up or down (`0` resets it).
    fn zoom(&mut self, step: i32, window: &mut Window, cx: &mut Context<Self>) {
        let at = theme::ZOOMS
            .iter()
            .position(|z| (*z - self.prefs.zoom).abs() < 0.001)
            .unwrap_or(2) as i32;
        let next = if step == 0 {
            2
        } else {
            (at + step).clamp(0, theme::ZOOMS.len() as i32 - 1)
        };
        let prefs = Prefs {
            zoom: theme::ZOOMS[next as usize],
            ..self.prefs
        };
        self.set_prefs(prefs, window, cx);
    }

    /// Apply and save new preferences.
    pub(crate) fn set_prefs(&mut self, prefs: Prefs, window: &mut Window, cx: &mut Context<Self>) {
        let old = std::mem::replace(&mut self.prefs, prefs);
        if prefs.accent != old.accent {
            theme::set_accent(prefs.accent, cx);
        }
        if prefs.layout != old.layout {
            theme::set_layout(prefs.layout, cx);
        }
        if prefs.theme != old.theme || prefs.accent != old.accent || prefs.layout != old.layout {
            theme::apply(prefs.theme.mode(window), cx);
        }
        let typeface = |p: &Prefs| (p.ui_font, p.code_font, p.zoom, p.editor_font);
        if prefs.code_theme != old.code_theme {
            theme::set_code_theme(prefs.code_theme);
            theme::apply(prefs.theme.mode(window), cx);
        }
        if typeface(&prefs) != typeface(&old) {
            theme::set_type(
                prefs.ui_font,
                prefs.code_font,
                prefs.zoom,
                prefs.editor_font,
            );
            theme::apply(prefs.theme.mode(window), cx);
            window.refresh();
        }
        if prefs.frosted != old.frosted {
            window.set_background_appearance(window_material(prefs.frosted));
        }
        prefs.save(self.store.as_deref());
        cx.notify();
    }

    pub(crate) fn open_settings(
        &mut self,
        _: &OpenSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings.is_some() {
            return;
        }
        let dialog = cx.new(|cx| SettingsDialog::new(self.prefs, window, cx));
        let subscription =
            cx.subscribe_in(&dialog, window, |this, _, event, window, cx| match event {
                SettingsEvent::Changed(prefs) => this.set_prefs(*prefs, window, cx),
                SettingsEvent::ClearHistory => {
                    if let Some(store) = &this.store
                        && let Err(err) = store.clear_history()
                    {
                        eprintln!("xenon: could not clear history: {err:#}");
                    }
                    this.reload_history(cx);
                }
                SettingsEvent::Dismiss => {
                    this.settings = None;
                    this.focus_editor(window, cx);
                    cx.notify();
                }
            });
        self.settings = Some((dialog, subscription));
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

    pub(crate) fn show_panel(
        &mut self,
        panel: SidePanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.show_side_panel && self.side_panel == panel {
            self.set_side_panel(false);
        } else {
            self.set_side_panel(true);
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
        self.set_side_panel(!self.show_side_panel);
        cx.notify();
    }

    pub(crate) fn classic(&self) -> bool {
        self.prefs.layout == Layout::Classic
    }

    /// The space around the card: none when the layout is Classic.
    fn card_gap(&self) -> f32 {
        if self.classic() { 0. } else { CARD_GAP }
    }

    fn set_side_panel(&mut self, show: bool) {
        if show != self.show_side_panel {
            self.show_side_panel = show;
            // Tests see panels where they end up, unless one asks for a slide.
            self.side_panel_since = (!cfg!(test)).then(std::time::Instant::now);
        }
    }

    /// Freeze the side panel partway through opening, for a snapshot.
    #[cfg(test)]
    pub(crate) fn slide_open_for_test(&mut self, seconds_ago: f32) {
        self.show_side_panel = true;
        self.side_panel_since =
            Some(std::time::Instant::now() - std::time::Duration::from_secs_f32(seconds_ago));
    }

    /// How far open the side panel is, from 0 to 1, easing as it slides.
    fn side_panel_open(&self, window: &Window) -> f32 {
        slide(self.side_panel_since, self.show_side_panel, window)
    }

    fn toggle_inspector(&mut self, _: &ToggleInspector, _: &mut Window, cx: &mut Context<Self>) {
        self.set_inspector(!self.show_inspector);
        cx.notify();
    }

    pub(crate) fn set_inspector(&mut self, show: bool) {
        if show != self.show_inspector {
            self.show_inspector = show;
            self.inspector_since = (!cfg!(test)).then(std::time::Instant::now);
        }
    }

    /// How far open the inspector is, from 0 to 1, easing as it slides.
    fn inspector_open(&self, window: &Window) -> f32 {
        slide(self.inspector_since, self.show_inspector, window)
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
            saved: focused(&self.saved_filter, cx),
            browse: self
                .active_tab()
                .browse
                .as_ref()
                .is_some_and(|b| focused(&b.filter, cx)),
        };
        let theme = cx.theme().clone();
        self.refresh_json_view(window, cx);
        self.refresh_definition(window, cx);

        let open = self.side_panel_open(window);
        let gap = self.card_gap();
        // What's left for the results, beside the rail, side panel and inspector.
        let beside = RAIL_WIDTH + (self.side_panel_width + gap) * open + 2. * gap;
        let fits = f32::from(window.viewport_size().width) - beside - MIN_RESULTS_WIDTH;
        self.inspector_shown
            .set(self.inspector_width.min(fits).max(MIN_INSPECTOR_WIDTH));
        let inspector_open = self.inspector_open(window);
        let inspector = if self.active_session().is_some() {
            self.inspector_shown.get() * inspector_open
        } else {
            0.
        };
        self.results_width.set(
            f32::from(window.viewport_size().width)
                - RAIL_WIDTH
                - (self.side_panel_width + gap) * open
                - inspector
                - 2. * gap,
        );
        let body = div()
            .flex()
            .flex_row()
            .flex_1()
            .min_h(px(0.))
            .pr(px(gap))
            .pb(px(gap))
            .child(self.render_rail(&theme, cx))
            .when(open > 0., |el| {
                let panel = self.render_side_panel(&theme, cx);
                el.child(if open < 1. {
                    // Sliding: clip the panel at its full width as the space grows.
                    div()
                        .w(px((self.side_panel_width + gap) * open))
                        .flex_none()
                        // A row, so the panel stretches to full height as it does unwrapped.
                        .flex()
                        .overflow_hidden()
                        // Fading a solid panel would show the darker frame through it.
                        .when(!self.classic(), |el| el.opacity(open))
                        .child(panel)
                        .into_any_element()
                } else {
                    panel.into_any_element()
                })
            })
            .child(
                card(&theme, self.classic())
                    .when(open >= 1., |el| el.ml(px(gap)))
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_row()
                    .child(self.render_center(&theme, cx))
                    .when(
                        inspector_open > 0. && self.active_session().is_some(),
                        |el| {
                            let panel = self.render_inspector(&theme, cx);
                            el.child(if inspector_open < 1. {
                                // Sliding: clip it at full width as the space grows.
                                div()
                                    .w(px(self.inspector_shown.get() * inspector_open))
                                    .flex_none()
                                    .flex()
                                    .justify_end()
                                    .overflow_hidden()
                                    .child(panel)
                                    .into_any_element()
                            } else {
                                panel.into_any_element()
                            })
                        },
                    ),
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
            .on_action(cx.listener(|this, _: &CopyMarkdown, _, cx| this.copy(Format::Markdown, cx)))
            .on_action(cx.listener(|this, _: &CopySql, _, cx| this.copy(Format::Sql, cx)))
            .on_action(cx.listener(|this, _: &ToggleStructure, _, cx| this.toggle_structure(cx)))
            .on_action(cx.listener(|this, _: &Quit, window, cx| {
                if this.may_quit(window, cx) {
                    cx.quit();
                }
            }))
            .on_action(
                cx.listener(|this, _: &SaveQueryAs, window, cx| this.save_query(true, window, cx)),
            )
            .on_action(
                // The query only: never staged changes (that's ⌘S).
                cx.listener(|this, _: &SaveQuery, window, cx| this.save_query(false, window, cx)),
            )
            .on_action(cx.listener(|this, _: &ShowSaved, window, cx| {
                this.show_panel(SidePanel::Saved, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ExportResults, _, cx| {
                if this.active_tab().grid.is_some() {
                    this.export_menu = !this.export_menu;
                    cx.notify();
                }
            }))
            .on_action(
                cx.listener(|this, _: &FocusEditor, window, cx| this.focus_editor(window, cx)),
            )
            .on_action(cx.listener(Self::open_palette))
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(Self::format_sql))
            .on_action(cx.listener(|this, _: &ZoomIn, window, cx| this.zoom(1, window, cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, window, cx| this.zoom(-1, window, cx)))
            .on_action(cx.listener(|this, _: &ZoomReset, window, cx| this.zoom(0, window, cx)))
            .on_action(
                cx.listener(|this, _: &ImportConnections, window, cx| this.open_import(window, cx)),
            )
            .on_action(cx.listener(Self::toggle_comment))
            .on_action(
                cx.listener(|this, _: &CheckForUpdates, _, cx| this.check_for_updates(true, cx)),
            )
            .on_action(cx.listener(Self::edit_cell))
            .on_action(cx.listener(Self::delete_rows))
            .on_action(cx.listener(Self::add_row))
            .on_action(cx.listener(Self::save_edits))
            .on_action(cx.listener(Self::discard_edits))
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
            .bg(if self.prefs.frosted {
                theme.frame
            } else {
                theme.bg
            })
            .font_family(crate::theme::ui_font())
            .text_color(theme.text)
            .text_size(crate::theme::text(12.))
            .relative()
            .child(self.render_titlebar(&theme, cx))
            .child(body)
            .child(self.render_statusbar(&theme, cx))
            .when_some(self.connect_dialog.as_ref(), |el, (dialog, _)| {
                el.child(dialog.clone())
            })
            .when_some(self.palette.as_ref(), |el, (palette, _)| {
                el.child(palette.clone())
            })
            .when_some(self.settings.as_ref(), |el, (settings, _)| {
                el.child(settings.clone())
            })
            .children(self.render_naming(&theme, cx))
            .children(self.render_parameters(&theme, cx))
            .children(self.render_import(&theme, cx))
            .children(self.render_column_menu(&theme, cx))
            .children(self.render_switcher(&theme, cx))
            .when_some(self.confirm.as_ref(), |el, (dialog, _, _)| {
                el.child(dialog.clone())
            })
    }
}

impl Workspace {
    fn render_titlebar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let several = self.sessions.len() > 1;
        let inset = drag_area(cx).w(px(TRAFFIC_LIGHT_INSET)).flex_none();
        let spacer = drag_area(cx).flex_1();
        let classic = self.classic();
        let tabs =
            self.tabs.iter().enumerate().map(|(index, tab)| {
                let active = index == self.active;
                let running = matches!(tab.query, Query::Running { .. });
                let session = tab.session.and_then(|id| self.session(id));
                let production =
                    session.is_some_and(|session| session.environment == Environment::Production);
                let mark =
                    match session {
                        Some(session) => widgets::engine_logo(
                            session.engine,
                            12.,
                            widgets::connection_color(&session.profile.color)
                                .unwrap_or(if active { theme.acc } else { theme.t3 }),
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
                    .max_w(px(TAB_MAX_WIDTH))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .relative()
                    .cursor_pointer()
                    // Classic: full-height tabs split by hairlines. Floating: pills.
                    .map(|el| {
                        if classic {
                            el.h_full()
                                .pl(px(14.))
                                .pr(px(8.))
                                .border_r_1()
                                .border_color(theme.line)
                                .when(active, |el| el.bg(theme.s1))
                        } else {
                            el.h(px(28.))
                                .pl(px(12.))
                                .pr(px(6.))
                                .rounded(px(8.))
                                .when(active, |el| el.bg(theme.pill))
                        }
                    })
                    .when(active, |el| el.text_color(theme.text))
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
                    // The active tab is underlined (Classic) or ringed (Floating, production only);
                    // red on production.
                    .when(active && classic, |el| {
                        el.child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom_0()
                                .h(px(2.))
                                .bg(if production { theme.err } else { theme.acc }),
                        )
                    })
                    .when(active && production && !classic, |el| {
                        el.border_1().border_color(theme.err)
                    })
                    // Every production tab is marked, more faintly when it isn't
                    // the one in front: one running there shows where it runs.
                    .when(!active && production && classic, |el| {
                        el.child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom_0()
                                .h(px(2.))
                                .bg(theme.err.opacity(0.45)),
                        )
                    })
                    .when(!active && production && !classic, |el| {
                        el.border_1().border_color(theme.err.opacity(0.45))
                    })
            });

        div()
            .id("titlebar")
            .h(px(TITLEBAR_HEIGHT))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .when(classic, |el| el.border_b_1().border_color(theme.line))
            .when(!classic, |el| el.gap(px(4.)))
            .on_mouse_move(
                cx.listener(|this, event: &gpui::MouseMoveEvent, window, _| {
                    if this.titlebar_pressed && event.dragging() {
                        this.titlebar_pressed = false;
                        window.start_window_move();
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.titlebar_pressed = false),
            )
            // Double-clicking empty title bar space zooms the window, like any Mac app.
            .on_click(|event: &ClickEvent, window, _| {
                if event.click_count() == 2 {
                    window.titlebar_double_click();
                }
            })
            .child(inset)
            .child(
                div()
                    .id("toggle-side-panel")
                    .map(|el| {
                        if classic {
                            el.h_full()
                                .px(px(10.))
                                .border_r_1()
                                .border_color(theme.line)
                        } else {
                            el.size(px(28.)).mr(px(4.)).rounded(px(8.))
                        }
                    })
                    .justify_center()
                    .flex()
                    .flex_none()
                    .items_center()
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
                        .items_center()
                        .gap(px(2.))
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
            .child(spacer)
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
                    .text_size(crate::theme::text(12.))
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
            .when(self.classic(), |el| {
                el.border_r_1().border_color(theme.line)
            })
            // Settings sits at the very bottom: level with the card's foot (Floating).
            .pb(px(if self.classic() { 6. } else { CARD_GAP }))
            .child(item("rail-explorer", "database", SidePanel::Explorer))
            .child(item("rail-history", "clock", SidePanel::History))
            .child(item("rail-saved", "bookmark", SidePanel::Saved))
            .child(div().flex_1())
            .child(
                div()
                    .id("rail-settings")
                    .size(px(36.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .rounded(px(6.))
                    .hover(|s| s.bg(theme.s3))
                    .tooltip(widgets::tooltip("Settings", "⌘,"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_settings(&OpenSettings, window, cx)
                    }))
                    .child(
                        svg()
                            .path(assets::icon("settings"))
                            .size(px(18.))
                            .text_color(theme.t2),
                    ),
            )
    }

    fn render_side_panel(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let (label, content) = match self.side_panel {
            SidePanel::Explorer => ("Explorer", self.render_explorer(theme, cx)),
            SidePanel::History => ("History", self.render_history(theme, cx)),
            SidePanel::Saved => ("Saved queries", self.render_saved_queries(theme, cx)),
        };
        let classic = self.classic();
        div()
            .w(px(self.side_panel_width))
            .flex_none()
            .relative()
            .flex()
            .flex_col()
            .when(classic, |el| {
                el.border_r_1()
                    .border_color(theme.line)
                    .when(!self.prefs.frosted, |el| el.bg(theme.s1))
            })
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
                    .map(|el| {
                        if classic {
                            el.right(px(-3.)).w(px(6.))
                        } else {
                            el.right(px(-CARD_GAP)).w(px(CARD_GAP))
                        }
                    })
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
        // The card clips square, so its painted corners round themselves.
        let inner = px(if self.classic() { 0. } else { CARD_RADIUS - 1. });
        let editor = div()
            .h(px(self.editor_height))
            .flex_none()
            .flex()
            .flex_col()
            .bg(theme.s2)
            .rounded_tl(inner)
            .when(!self.show_inspector, |el| el.rounded_tr(inner))
            .border_b_1()
            .border_color(theme.line)
            .child(self.render_toolbar(theme, cx))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .pt(px(6.))
                    .font_family(crate::theme::mono_font())
                    .text_size(crate::theme::text(self.prefs.editor_font))
                    .child(Editor::new(&tab.editor).h_full().appearance(false)),
            );

        // Room at the foot so highlighted rows stop short of the rounded corners.
        let results = div()
            .flex_1()
            .min_h(px(0.))
            .when(!self.classic(), |el| el.pb(px(8.)))
            .flex()
            .flex_col();
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
            .child(
                results
                    .when_some(tab.browse.as_ref(), |el, browse| {
                        el.child(self.render_filter_bar(browse, theme, cx))
                    })
                    .children(
                        tab.browse
                            .as_ref()
                            .filter(|b| !b.structure)
                            .and_then(|_| self.render_filter_builder(theme, cx)),
                    )
                    .map(|el| match tab.browse.as_ref().filter(|b| b.structure) {
                        Some(browse) => el.child(
                            div()
                                .flex_1()
                                .min_h(px(0.))
                                .child(self.render_structure(browse, theme, cx)),
                        ),
                        None => el
                            .child(self.render_results_bar(theme, cx))
                            .children(self.render_result_sets(theme, cx))
                            .child(
                                div()
                                    .flex_1()
                                    .min_h(px(0.))
                                    .child(self.render_results(theme, cx)),
                            ),
                    }),
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
            .font_family(crate::theme::ui_font())
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
            let saved = tab.saved_query.is_some();
            bar = bar
                .child(
                    small(widgets::button(
                        theme,
                        "save-query",
                        "Save",
                        ButtonKind::Ghost,
                        true,
                    ))
                    .tooltip(widgets::tooltip(
                        if saved {
                            "Update saved query"
                        } else {
                            "Save query"
                        },
                        "⌘S",
                    ))
                    .on_click(cx.listener(|this, _, w, cx| this.save_query(false, w, cx))),
                )
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
                    .on_click(cx.listener(|this, _, w, cx| this.run_all(&RunAll, w, cx))),
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
                    .on_click(cx.listener(|this, _, w, cx| this.run_statement(&RunQuery, w, cx))),
                );
        }
        bar
    }

    fn render_filter_bar(
        &self,
        browse: &Browse,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let sorted = browse.order.as_ref().map(|(column, direction)| {
            let arrow = match direction {
                SortDirection::Ascending => "▲",
                SortDirection::Descending => "▼",
            };
            format!("order by {column} {arrow}")
        });
        let active = sorted.is_some() || !browse.filter.read(cx).value().trim().is_empty();
        let structure = browse.structure;
        let this = cx.entity().downgrade();
        let toggle = widgets::segmented(
            theme,
            "table-view",
            vec![("Data", !structure), ("Structure", structure)],
            move |ix, _, cx| {
                this.update(cx, |this, cx| {
                    if (*ix == 1) != structure {
                        this.toggle_structure(cx);
                    }
                })
                .ok();
            },
        )
        .w(px(170.))
        .flex_none();

        let bar = div()
            .h(px(44.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(8.))
            .border_b_1()
            .border_color(theme.line)
            .child(toggle);
        if structure {
            return bar
                .child(div().flex_1())
                .child(
                    widgets::button(
                        theme,
                        "refresh-structure",
                        "Refresh",
                        ButtonKind::Ghost,
                        true,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.load_table_info(cx))),
                )
                .into_any_element();
        }
        bar.child(
            widgets::button(
                theme,
                "filter-builder",
                "+ Filter",
                if browse.builder.is_some() {
                    ButtonKind::Secondary
                } else {
                    ButtonKind::Ghost
                },
                true,
            )
            .h(px(30.))
            .tooltip(widgets::tooltip(
                "Build a filter: column, condition, value",
                "",
            ))
            .on_click(cx.listener(|this, _, window, cx| this.toggle_filter_builder(window, cx))),
        )
        .child(
            field(
                theme,
                Some(
                    div()
                        .font_family(crate::theme::mono_font())
                        .text_size(crate::theme::text(10.5))
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
                    .font_family(crate::theme::mono_font())
                    .text_size(crate::theme::text(10.5))
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
        .into_any_element()
    }

    fn render_structure(
        &self,
        browse: &Browse,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(info) = &browse.info else {
            return match &browse.info_error {
                Some(error) => div()
                    .p(px(16.))
                    .text_size(crate::theme::text(12.))
                    .text_color(theme.err)
                    .child(error.clone())
                    .into_any_element(),
                None => centered_hint(theme, "Loading structure…"),
            };
        };
        let Some(session) = self.active_session() else {
            return centered_hint(theme, "Not connected.");
        };
        let is_view = session
            .schemas
            .iter()
            .find(|s| s.name == browse.schema)
            .and_then(|s| s.objects.iter().find(|o| o.name == browse.table))
            .is_some_and(|o| o.kind == ObjectKind::View);
        let conn = session.conn.clone();
        let this = cx.entity().downgrade();
        crate::structure::render(
            crate::structure::View {
                info,
                table: &browse.table,
                is_view,
                editor: browse.definition.as_ref().map(|(_, e)| e.clone()),
                ident: &|name| conn.ident(name),
                open: Rc::new(move |schema, table, window, cx| {
                    this.update(cx, |this, cx| this.open_table(schema, table, window, cx))
                        .ok();
                }),
            },
            theme,
        )
    }

    /// The cell being edited: its column, its value in the editor its type
    /// calls for, and what to do with it.
    /// Tabs for each result of a script that returned several.
    fn render_result_sets(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let tab = self.active_tab();
        if tab.sets.len() < 2 {
            return None;
        }
        let rows = |ix: usize| {
            let grid = if ix == tab.shown_set {
                tab.grid.as_ref()
            } else {
                tab.sets[ix].as_ref()
            };
            grid.map_or(0, |g| g.data.rows.len())
        };
        Some(
            div()
                .id("result-sets")
                .flex()
                .flex_none()
                .gap(px(4.))
                .px(px(10.))
                .py(px(6.))
                .overflow_x_scroll()
                .border_b_1()
                .border_color(theme.line_2)
                .children((0..tab.sets.len()).map(|ix| {
                    let on = ix == tab.shown_set;
                    let count = rows(ix);
                    div()
                        .id(("result-set", ix))
                        .flex_none()
                        .h(px(24.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .rounded(px(RADIUS_CONTROL))
                        .cursor_pointer()
                        .when(on, |el| el.bg(theme.s3).text_color(theme.text))
                        .when(!on, |el| {
                            el.text_color(theme.t3).hover(|s| s.text_color(theme.t2))
                        })
                        .on_click(cx.listener(move |this, _, _, cx| this.show_set(ix, cx)))
                        // The last is the script's last, whatever was left out.
                        .child(format!(
                            "Result {}",
                            if ix + 1 == tab.sets.len() {
                                tab.sets_total.max(ix + 1)
                            } else {
                                ix + 1
                            }
                        ))
                        .child(
                            div()
                                .font_family(crate::theme::mono_font())
                                .text_size(crate::theme::text(10.5))
                                .text_color(theme.t3)
                                .child(format!(
                                    "{} {}",
                                    group_digits(count),
                                    if count == 1 { "row" } else { "rows" }
                                )),
                        )
                }))
                .into_any_element(),
        )
    }

    /// Show another of the last script's results.
    pub(crate) fn show_set(&mut self, ix: usize, cx: &mut Context<Self>) {
        let tab = &mut self.tabs[self.active];
        if ix == tab.shown_set || ix >= tab.sets.len() || !tab.edits.is_empty() {
            return;
        }
        let shown = tab.shown_set;
        tab.sets[shown] = tab.grid.take();
        tab.grid = tab.sets[ix].take();
        tab.shown_set = ix;
        tab.selection = None;
        tab.editing = None;
        cx.notify();
    }

    /// The open cell edit, as its inspector field shows it: the value's own
    /// editor (a text area, choices, true / false), or, for one typed into its
    /// cell, the buttons that finish it. Nothing above the grid moves.
    fn render_field_editor(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        use crate::value_editor::ValueKind;
        let editor = self.active_tab().editing.as_ref()?;
        let small = |button: gpui::Stateful<gpui::Div>| {
            button
                .h(px(24.))
                .px(px(9.))
                .text_size(crate::theme::text(11.5))
        };
        let set_null = small(widgets::button(
            theme,
            "cell-null",
            "Set NULL",
            ButtonKind::Ghost,
            true,
        ))
        .on_click(cx.listener(|this, _, window, cx| this.commit_edit(true, window, cx)));
        let cancel = small(widgets::button(
            theme,
            "cell-cancel",
            "Cancel",
            ButtonKind::Ghost,
            true,
        ))
        .tooltip(widgets::tooltip("Leave it as it was", "esc"))
        .on_click(cx.listener(|this, _, window, cx| this.cancel_edit(window, cx)));
        let done = |keys: &'static str| {
            small(widgets::button(
                theme,
                "cell-done",
                "Done",
                ButtonKind::Primary,
                true,
            ))
            .pl(px(9.))
            .child(kbd_on_accent(theme, keys))
            .on_click(cx.listener(|this, _, window, cx| this.commit_edit(false, window, cx)))
        };
        let buttons = |done_keys: Option<&'static str>| {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(4.))
                .child(set_null)
                .child(cancel)
                .child(div().flex_1())
                .children(done_keys.map(done))
        };
        let choice =
            |id: (&'static str, usize), shown: SharedString, stored: String, on: bool| {
                small(widgets::button(
                    theme,
                    id,
                    shown,
                    if on {
                        ButtonKind::Primary
                    } else {
                        ButtonKind::Secondary
                    },
                    true,
                ))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.commit_text(stored.clone(), window, cx)
                }))
            };
        let current = editor.input.read(cx).value().to_string();
        let body = div()
            .key_context("CellEditor")
            .on_action(
                cx.listener(|this, _: &gpui_component::input::Escape, window, cx| {
                    this.cancel_edit(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &crate::CommitCell, window, cx| {
                this.commit_edit(false, window, cx)
            }))
            .flex()
            .flex_col()
            .gap(px(8.))
            .font_family(crate::theme::ui_font());
        let body = match &editor.kind {
            // Long text, or JSON (checked when you press Done).
            ValueKind::Text | ValueKind::Json => {
                let area = editor.area.as_ref()?;
                body.child(
                    div()
                        .h(px(220.))
                        .rounded(px(widgets::RADIUS_CONTROL))
                        .border_1()
                        .border_color(theme.acc)
                        .bg(theme.field)
                        .p(px(4.))
                        .child(
                            Editor::new(area)
                                .h_full()
                                .appearance(false)
                                .font_family(crate::theme::mono_font())
                                .text_size(crate::theme::text(12.)),
                        ),
                )
                .child(buttons(Some("⌘↵")))
            }
            ValueKind::Choice(options) => body
                .child(div().flex().flex_wrap().gap(px(4.)).children(
                    options.iter().enumerate().map(|(ix, option)| {
                        choice(
                            ("cell-option", ix),
                            option.clone().into(),
                            option.clone(),
                            *option == current,
                        )
                    }),
                ))
                .child(buttons(None)),
            ValueKind::Bool { numeric } => {
                let (yes, no) = if *numeric {
                    ("1", "0")
                } else {
                    ("true", "false")
                };
                body.child(
                    div()
                        .flex()
                        .gap(px(4.))
                        .child(choice(
                            ("cell-true", 0),
                            "true".into(),
                            yes.into(),
                            current == yes || current == "true",
                        ))
                        .child(choice(
                            ("cell-false", 0),
                            "false".into(),
                            no.into(),
                            current == no || current == "false",
                        )),
                )
                .child(buttons(None))
            }
            // Typed into its cell; here, how it's written (dates) and the buttons.
            ValueKind::Line | ValueKind::Date(_) => {
                let date = match &editor.kind {
                    ValueKind::Date(kind) => Some(*kind),
                    _ => None,
                };
                body.when_some(date, |el, kind| {
                    el.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .font_family(crate::theme::mono_font())
                                    .text_size(crate::theme::text(10.5))
                                    .text_color(theme.t3)
                                    .child(kind.hint()),
                            )
                            .child(
                                small(widgets::button(
                                    theme,
                                    "cell-now",
                                    "Now",
                                    ButtonKind::Ghost,
                                    true,
                                ))
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.fill_cell(kind.now(), window, cx)
                                    },
                                )),
                            ),
                    )
                })
                .child(buttons(Some("↵")))
            }
        };
        Some(body.into_any_element())
    }

    /// The cell being typed into in place (single-line values): its display row
    /// and column in the grid, and its field.
    pub(crate) fn inline_edit(&self) -> Option<(usize, usize, Entity<InputState>)> {
        use crate::value_editor::ValueKind;
        let tab = self.active_tab();
        let editor = tab.editing.as_ref()?;
        if !matches!(editor.kind, ValueKind::Line | ValueKind::Date(_)) {
            return None;
        }
        let grid = tab.grid.as_ref()?;
        let row = match editor.row {
            RowRef::Existing(r) => grid.order().iter().position(|&x| x == r)?,
            RowRef::New(n) => grid.row_count() + n,
        };
        Some((row, grid.display_of(editor.column)?, editor.input.clone()))
    }

    /// Tab while typing into a cell: stage it, then edit the next (or, with
    /// Shift, the previous) cell in the row.
    pub(crate) fn edit_next_cell(
        &mut self,
        back: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((row, column, _)) = self.inline_edit() else {
            return;
        };
        self.commit_edit(false, window, cx);
        if self.active_tab().editing.is_some() {
            // Not staged (a value the column can't take): stay put.
            return;
        }
        let Some(grid) = self.active_tab().grid.as_ref() else {
            return;
        };
        let next = if back {
            column.checked_sub(1)
        } else {
            Some(column + 1).filter(|c| *c < grid.columns().len())
        };
        if let Some(data) = next.and_then(|c| grid.column(c)) {
            self.begin_edit(row, data, window, cx);
        }
    }

    /// Put `text` in the open edit bar's field.
    fn fill_cell(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = &self.tabs[self.active].editing {
            let input = editor.input.clone();
            input.update(cx, |input, cx| input.set_value(text, window, cx));
        }
    }

    /// Stage `text` as the cell's value (a choice, true / false).
    pub(crate) fn commit_text(
        &mut self,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.fill_cell(text, window, cx);
        self.commit_edit(false, window, cx);
    }

    /// The right of the results bar: staged changes and how to save them, or
    /// how to start editing, or why this table can't be edited.
    /// The tab's note (saved, an error, why it can't be edited): it gives way,
    /// cut short, so the controls on the right always fit.
    fn render_note(&self, theme: &Theme) -> Option<AnyElement> {
        let tab = self.active_tab();
        let (text, color) = match &tab.flash {
            Some((message, error)) => (message.clone(), if *error { theme.err } else { theme.ok }),
            None if tab.saving => ("Saving…".into(), theme.t2),
            None => match self.edit_target() {
                Err(why) if tab.browse.is_some() && tab.grid.is_some() && tab.edits.is_empty() => {
                    (why.into(), theme.t3)
                }
                _ => return None,
            },
        };
        Some(
            div()
                .flex_1()
                .min_w(px(0.))
                .truncate()
                .text_color(color)
                .child(text)
                .into_any_element(),
        )
    }

    /// Whether the results are narrow enough that the bar's controls go compact.
    pub(crate) fn compact_bar(&self) -> bool {
        self.results_width.get() < ROOMY_RESULTS
    }

    /// Staged changes and what to do with them, or the button that adds a row.
    fn render_edit_actions(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let tab = self.active_tab();
        let compact = self.compact_bar();
        let small = |button: gpui::Stateful<gpui::Div>| {
            button
                .h(px(24.))
                .px(px(9.))
                .text_size(crate::theme::text(11.5))
        };
        if tab.saving {
            return None;
        }
        if !tab.edits.is_empty() {
            let count = tab.edits.count();
            return Some(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(4.))
                    .child(div().size(px(7.)).rounded_full().bg(theme.warn))
                    .child(
                        div()
                            .mr(px(4.))
                            .whitespace_nowrap()
                            .text_color(theme.text)
                            .font_weight(FontWeight::MEDIUM)
                            .child(if count == 1 {
                                "1 change".to_string()
                            } else {
                                format!("{count} changes")
                            }),
                    )
                    .child(
                        small(widgets::button(
                            theme,
                            "discard-edits",
                            "Discard",
                            ButtonKind::Ghost,
                            true,
                        ))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.discard_edits(&DiscardEdits, window, cx)
                        })),
                    )
                    .child(
                        small(widgets::button(
                            theme,
                            "review-edits",
                            if compact { "Review" } else { "Review SQL" },
                            ButtonKind::Ghost,
                            true,
                        ))
                        .tooltip(widgets::tooltip("Review the SQL that saves them", ""))
                        .on_click(cx.listener(|this, _, window, cx| this.review_edits(window, cx))),
                    )
                    .child(
                        small(widgets::button(
                            theme,
                            "save-edits",
                            "Save",
                            ButtonKind::Primary,
                            true,
                        ))
                        .pl(px(9.))
                        .when(!compact, |el| el.child(kbd_on_accent(theme, "⌘S")))
                        .when(compact, |el| {
                            el.tooltip(widgets::tooltip("Save changes", "⌘S"))
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.save_edits(&SaveEdits, window, cx)
                        })),
                    )
                    .into_any_element(),
            );
        }
        self.edit_target().ok()?;
        Some(
            icon_button(theme, "add-row", "plus", false)
                .tooltip(widgets::tooltip(
                    "Add a row (double-click a cell to edit one)",
                    "",
                ))
                .on_click(cx.listener(|this, _, window, cx| this.add_row(&AddRow, window, cx)))
                .into_any_element(),
        )
    }

    fn render_results_bar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = self.active_tab();
        // A status dot and word, then quieter facts about the result.
        let (dot, status): (Rgba, SharedString) = match &tab.query {
            Query::Idle => (theme.t3, "No results".into()),
            Query::Running { .. } if tab.fetched > 0 => (
                theme.acc,
                format!("{} rows so far…", group_digits(tab.fetched)).into(),
            ),
            Query::Running { .. } => (theme.acc, "Running…".into()),
            Query::Failed(_) => (theme.err, "Error".into()),
            Query::Done(result) if result.cancelled => (
                theme.warn,
                match &tab.grid {
                    Some(grid) => format!("Stopped at {} rows", group_digits(grid.data.rows.len())),
                    None => "Stopped".into(),
                }
                .into(),
            ),
            Query::Done(result) => (
                theme.ok,
                match (&result.outcome, &tab.grid) {
                    (_, Some(grid)) => {
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
                        let noun = if *n == 1 { "row" } else { "rows" };
                        format!("{} {noun} affected", group_digits(*n as usize))
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
        let can_export = matches!(tab.query, Query::Done(_) | Query::Idle)
            && tab
                .grid
                .as_ref()
                .is_some_and(|grid| !grid.data.columns.is_empty());
        // A capped query result can be run again keeping every row.
        let can_fetch_all = tab.browse.is_none()
            && tab.last_run.is_some()
            && matches!(&tab.query, Query::Done(result) if !result.cancelled)
            && tab.grid.as_ref().is_some_and(|grid| grid.data.truncated);

        // Left: what happened, giving way (cut short) when space runs out; the
        // status itself always shows.
        let note = tab.flash.as_ref().map(|(text, _)| text.clone());
        let left = div()
            .id("results-status")
            // Cut short, a note can still be read in full on hover.
            .when_some(note, |el, note| el.tooltip(widgets::tooltip(note, "")))
            .flex_1()
            .min_w(px(90.))
            .flex()
            .items_center()
            .gap(px(10.))
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(7.))
                    .whitespace_nowrap()
                    .text_color(theme.text)
                    .font_weight(FontWeight::MEDIUM)
                    .child(div().size(px(7.)).rounded_full().bg(dot))
                    .child(status),
            )
            .children(facts.into_iter().map(|fact| {
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .font_family(crate::theme::mono_font())
                    .text_size(crate::theme::text(11.))
                    .text_color(theme.t3)
                    .child(fact)
            }))
            .when(matches!(tab.query, Query::Running { .. }), |el| {
                el.child(
                    widgets::with_keys(theme, &[("⌘.", true), ("to cancel", false)])
                        .flex_none()
                        .text_size(crate::theme::text(11.))
                        .text_color(theme.t3),
                )
            })
            .when(can_fetch_all, |el| {
                el.child(
                    widgets::button(theme, "fetch-all", "Fetch all", ButtonKind::Ghost, true)
                        .flex_none()
                        .h(px(22.))
                        .px(px(8.))
                        .on_click(cx.listener(|this, _, w, cx| this.fetch_all(w, cx))),
                )
            })
            .children(self.render_note(theme));
        // Right: the controls, always whole.
        let right = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .children(self.render_edit_actions(theme, cx))
            .children(self.render_pager(theme, cx))
            .when(can_export, |el| {
                el.child(self.render_view_switch(theme, cx))
                    .child(self.render_export(theme, cx))
            });
        div()
            .h(px(32.))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.))
            .px(px(14.))
            .overflow_hidden()
            .border_b_1()
            .border_color(theme.line)
            .bg(theme.s1)
            .text_size(crate::theme::text(11.5))
            .child(left)
            .child(right)
    }

    /// The Export button and, when open, its menu of formats.
    fn render_export(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let truncated = self
            .active_tab()
            .grid
            .as_ref()
            .is_some_and(|grid| grid.data.truncated);
        let menu = widgets::overlay_panel(theme)
            .id("export-menu")
            .occlude()
            .w(px(200.))
            .p(px(4.))
            .text_size(crate::theme::text(12.5))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.export_menu = false;
                cx.notify();
            }))
            .child(
                div()
                    .px(px(8.))
                    .pt(px(4.))
                    .pb(px(6.))
                    .text_size(crate::theme::text(11.))
                    .text_color(theme.t3)
                    .child("Export results as"),
            )
            .children(crate::export::FORMATS.iter().map(|&(format, label)| {
                div()
                    .id(label)
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(26.))
                    .px(px(8.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .text_color(theme.text)
                    .hover(|el| el.bg(theme.s2))
                    .child(label)
                    .child(
                        div()
                            .font_family(crate::theme::mono_font())
                            .text_size(crate::theme::text(11.))
                            .text_color(theme.t3)
                            .child(format!(".{}", format.extension())),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.export(format, cx)))
            }))
            .when(truncated, |el| {
                el.child(
                    div()
                        .mt(px(4.))
                        .px(px(8.))
                        .py(px(6.))
                        .border_t_1()
                        .border_color(theme.line)
                        .text_size(crate::theme::text(11.))
                        .text_color(theme.t3)
                        .child("Only the rows loaded are exported. Fetch all first to export every row."),
                )
            });
        div()
            .relative()
            .child(
                icon_button(theme, "export", "download", self.export_menu)
                    .tooltip(|window, cx| widgets::tooltip("Export results", "⇧⌘S")(window, cx))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.export_menu = !this.export_menu;
                        cx.notify();
                    })),
            )
            .when(self.export_menu, |el| {
                el.child(gpui::deferred(
                    div().absolute().top(px(26.)).right_0().child(menu),
                ))
            })
    }

    fn render_results(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let tab = self.active_tab();
        if let Some(grid) = &tab.grid {
            if grid.data.columns.is_empty() {
                return centered_hint(theme, "The query returned no columns.");
            }
            match tab.view {
                crate::views::ResultView::Records => return self.render_records(grid, theme),
                crate::views::ResultView::Json => return self.render_json(theme, cx),
                crate::views::ResultView::Grid => {}
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
                edit: Rc::new({
                    let this = this.clone();
                    move |row, column, window, cx| {
                        this.update(cx, |this, cx| this.begin_edit(row, column, window, cx))
                            .ok();
                    }
                }),
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
                follow: Rc::new({
                    let this = this.clone();
                    move |row, column, window, cx| {
                        this.update(cx, |this, cx| this.follow_key(row, column, window, cx))
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
                column_menu: Rc::new({
                    let this = this.clone();
                    move |column, at, _, cx| {
                        this.update(cx, |this, cx| this.open_column_menu(column, at, cx))
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

            let links: Vec<Option<SharedString>> = grid
                .data
                .columns
                .iter()
                .map(|column| {
                    let browse = tab.browse.as_ref()?;
                    Some(browse.key_for(&column.name)?.ref_table.clone().into())
                })
                .collect();
            // Room for a foreign key's arrow and the table it points at.
            let manual = tab.manual_widths(grid);
            let mut widths = widths;
            for (ix, link) in links.iter().enumerate() {
                if let (Some(table), None) = (link, manual.get(ix).copied().flatten()) {
                    let type_name = grid.data.columns[ix].type_name.as_deref().unwrap_or("");
                    let header = (type_name.chars().count() + 3 + table.chars().count()) as f32;
                    widths[ix] = (widths[ix] + 18.).max(header * 6. + 24.);
                }
            }
            let view = results::View {
                grid,
                widths,
                selection: tab.selection,
                sort,
                edits: tab.edits.clone(),
                links,
                editing: self.inline_edit(),
            };
            return div()
                .id("results")
                .key_context(if self.inline_edit().is_some() {
                    "ResultsGrid editing"
                } else {
                    "ResultsGrid"
                })
                .track_focus(&self.grid_focus)
                // Typing into a cell: Escape cancels, Tab saves it and moves on.
                .on_action(
                    cx.listener(|this, _: &gpui_component::input::Escape, window, cx| {
                        if this.inline_edit().is_some() {
                            this.cancel_edit(window, cx);
                        } else {
                            cx.propagate();
                        }
                    }),
                )
                .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                    if event.keystroke.key == "tab" && this.inline_edit().is_some() {
                        cx.stop_propagation();
                        this.edit_next_cell(event.keystroke.modifiers.shift, window, cx);
                    }
                }))
                .size_full()
                .child(results::render(view, theme, callbacks))
                .into_any_element();
        }

        match &tab.query {
            Query::Failed(message) => {
                div()
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
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .child(
                                div()
                                    .font_family(crate::theme::mono_font())
                                    .text_size(crate::theme::text(11.5))
                                    .line_height(px(18.))
                                    .text_color(theme.text)
                                    .child(message.clone()),
                            )
                            .children(tab.error_hint.as_ref().and_then(|hint| {
                                let suggestion = hint.suggestion.clone()?;
                                Some(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(10.))
                                        .child(div().text_color(theme.t2).child("Did you mean"))
                                        .child(
                                            div()
                                                .font_family(crate::theme::mono_font())
                                                .text_color(theme.text)
                                                .child(suggestion),
                                        )
                                        .when(hint.at.is_some(), |el| {
                                            el.child(
                                                widgets::button(
                                                    theme,
                                                    "fix-error",
                                                    "Fix",
                                                    ButtonKind::Secondary,
                                                    true,
                                                )
                                                .h(px(24.))
                                                .px(px(10.))
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.fix_error(window, cx)
                                                })),
                                            )
                                        }),
                                )
                            })),
                    )
                    .into_any_element()
            }
            Query::Running { .. } => centered_hint(theme, "Running…"),
            Query::Done(_) => centered_hint(theme, "Done."),
            Query::Idle if self.active_session().is_some() => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(10.))
                .text_color(theme.t3)
                .child(widgets::with_keys(
                    theme,
                    &[("⌘↵", true), ("runs the statement under the cursor", false)],
                ))
                .child(widgets::with_keys(
                    theme,
                    &[("⇧⌘↵", true), ("runs everything", false)],
                ))
                .into_any_element(),
            Query::Idle => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .child(
                    div()
                        .text_size(crate::theme::text(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Xenon"),
                )
                .child(
                    div()
                        .text_color(theme.t3)
                        .child("Open a database to get started."),
                )
                .into_any_element(),
        }
    }

    fn render_inspector(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = self.active_tab();
        let mut panel = div()
            .w(px(self.inspector_shown.get()))
            .flex_none()
            .relative()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(theme.line)
            .child(
                // Drag the edge to resize; double-click to reset.
                div()
                    .id("inspector-edge")
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(-3.))
                    .w(px(6.))
                    .cursor_col_resize()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                            if event.click_count >= 2 {
                                this.inspector_width = INSPECTOR_WIDTH;
                                cx.notify();
                            } else {
                                this.drag = Some(Drag::Inspector {
                                    start_x: event.position.x,
                                    start_width: this.inspector_width,
                                });
                            }
                        }),
                    ),
            );

        // The row as it will be once saved: staged values in place of fetched ones.
        let selected = tab
            .selection
            .zip(tab.grid.as_ref())
            .and_then(|(selection, grid)| {
                let ix = selection.head.0;
                let row_ref = Self::row_ref(tab, ix)?;
                let values: Vec<(Option<Value>, bool)> = (0..grid.data.columns.len())
                    .map(|col| match (tab.edits.value(row_ref, col), row_ref) {
                        (Some(value), _) => (value.cloned(), true),
                        (None, RowRef::Existing(r)) => {
                            (Some(grid.data.rows[r][col].clone()), false)
                        }
                        (None, RowRef::New(_)) => (None, false),
                    })
                    .collect();
                Some((ix, row_ref, values, &grid.data.columns))
            });
        let Some((ix, row_ref, row, columns)) = selected else {
            return panel
                .child(panel_label(theme, "Inspector"))
                .child(empty_hint(theme, "Select a row to see every field"));
        };

        let label = match row_ref {
            RowRef::New(_) => "New row".to_string(),
            RowRef::Existing(r) if tab.edits.deleted.contains(&r) => {
                format!("Row {} · to delete", ix + 1)
            }
            RowRef::Existing(_) => format!("Row {}", ix + 1),
        };
        panel = panel.child(panel_label(theme, &label));
        // The cell being edited is edited here, in its field (or typed into its
        // cell, with the buttons here).
        let editing = tab.editing.as_ref().map(|e| {
            (
                e.column,
                e.row,
                matches!(
                    e.kind,
                    crate::value_editor::ValueKind::Line | crate::value_editor::ValueKind::Date(_)
                ),
                e.input.read(cx).value().to_string(),
            )
        });
        let mut field_editor = self.render_field_editor(theme, cx);
        let fields = div()
            .id("inspector-fields")
            .flex_1()
            .overflow_y_scroll()
            .children(columns.iter().zip(row).enumerate().map(
                |(col, (column, (value, edited)))| {
                    // A foreign key with a value: a link to the row it points at.
                    let target = tab
                        .browse
                        .as_ref()
                        .and_then(|browse| browse.key_for(&column.name))
                        .filter(|_| matches!(row_ref, RowRef::Existing(_)))
                        .filter(|_| matches!(&value, Some(v) if !v.is_null()))
                        .map(|key| key.ref_table.clone());
                    let here = editing
                        .as_ref()
                        .filter(|(c, r, _, _)| *c == col && *r == row_ref);
                    div()
                        .px(px(12.))
                        .py(px(8.))
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .border_b_1()
                        .border_color(theme.line_2)
                        // The field being edited: its tint is the only mark.
                        .when(here.is_some(), |el| el.bg(theme.acc_wash))
                        .font_family(crate::theme::mono_font())
                        .text_size(crate::theme::text(11.5))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .font_family(crate::theme::ui_font())
                                .text_size(crate::theme::text(11.))
                                .text_color(theme.t3)
                                .child(SharedString::from(column.name.clone()))
                                .child(div().flex_1())
                                .when_some(target, |el, table| {
                                    el.child(
                                        div()
                                            .id(("follow", col))
                                            .flex()
                                            .items_center()
                                            .gap(px(3.))
                                            .cursor_pointer()
                                            .text_color(theme.acc)
                                            .hover(|el| el.underline())
                                            .child(SharedString::from(format!("Open in {table}")))
                                            .child(
                                                gpui::svg()
                                                    .path(assets::icon("arrow-up-right"))
                                                    .size(px(11.))
                                                    .text_color(theme.acc),
                                            )
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.follow_key(ix, col, window, cx)
                                            })),
                                    )
                                }),
                        )
                        .children(here.map(|(_, _, inline, typed)| {
                            let mut el = div().flex().flex_col().gap(px(6.));
                            if *inline {
                                // What's being typed in the cell, as it goes.
                                el = el.child(div().text_color(theme.text).child(
                                    if typed.is_empty() {
                                        SharedString::new_static("(empty)")
                                    } else {
                                        SharedString::from(typed.clone())
                                    },
                                ));
                            }
                            el.children(field_editor.take())
                        }))
                        .when(here.is_none(), |el| {
                            el.child(
                                match value {
                                    None => div().italic().text_color(theme.t3).child("default"),
                                    Some(Value::Null) => {
                                        div().italic().text_color(theme.t3).child("NULL")
                                    }
                                    Some(value) => {
                                        let text = value.display();
                                        let shown =
                                            pretty_json(&text).unwrap_or_else(|| text.into_owned());
                                        div().child(SharedString::from(shown))
                                    }
                                }
                                .when(edited, |el| el.text_color(theme.warn)),
                            )
                        })
                },
            ));
        panel.child(fields)
    }

    fn render_statusbar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
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
            .when(self.classic(), |el| {
                el.border_t_1().border_color(theme.line)
            })
            .font_family(crate::theme::mono_font())
            .text_size(crate::theme::text(10.5));

        bar = match self.active_session() {
            Some(session) => bar
                .child(
                    cell("".into(), theme.t2)
                        .id("connection-menu")
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.s3))
                        .when(self.switcher, |el| el.bg(theme.s3))
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_switcher(cx)))
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
                        )
                        // Production over TLS that doesn't check the server.
                        .when(
                            session.environment == Environment::Production
                                && !session.tls_verified
                                && !session.conn.is_local(),
                            |el| el.child(env_tag("TLS UNVERIFIED", theme.warn)),
                        ),
                )
                .child(cell(session.version.clone().into(), theme.t3))
                .when_some(session.conn.tunnel(), |bar, via| {
                    bar.child(cell(format!("ssh via {via}").into(), theme.t3))
                })
                .map(|bar| {
                    if !session.in_transaction {
                        return bar.child(cell(
                            if session.read_only {
                                "read-only"
                            } else {
                                "auto-commit"
                            }
                            .into(),
                            theme.t3,
                        ));
                    }
                    // Locks are held until this ends: say so, and offer to end it.
                    let action = |id: &'static str, label: &'static str, commit: bool| {
                        div()
                            .id(id)
                            .px(px(6.))
                            .rounded(px(3.))
                            .cursor_pointer()
                            .text_color(theme.text)
                            .hover(|el| el.bg(theme.s3))
                            .child(label)
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.end_transaction(commit, cx)),
                            )
                    };
                    bar.child(
                        cell("".into(), theme.warn)
                            .gap(px(6.))
                            .bg(Rgba {
                                a: 0.12,
                                ..theme.warn
                            })
                            .child(div().size(px(6.)).rounded_full().bg(theme.warn))
                            .child("transaction open")
                            .child(action("txn-commit", "Commit", true))
                            .child(action("txn-rollback", "Roll back", false)),
                    )
                }),
            None => bar.child(
                cell("no connection".into(), theme.t3)
                    .id("connection-menu")
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.s3))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_switcher(cx))),
            ),
        };
        let open = self.sessions.len();
        bar = bar.when(open > 1, |bar| {
            bar.child(div().flex_1())
                .child(cell(format!("{open} connections open").into(), theme.t3).border_l_1())
        });

        let totals = self.selection_totals().map(|stats| {
            let text = match &stats.numbers {
                Some(n) => format!(
                    "count {} · sum {} · avg {} · min {} · max {}",
                    group_digits(stats.count),
                    n.sum,
                    n.average,
                    n.min,
                    n.max
                ),
                None => format!("count {}", group_digits(stats.count)),
            };
            div()
                .h_full()
                .border_l_1()
                .border_color(theme.line)
                .child(cell(text.into(), theme.t2))
        });
        let update = match &self.update {
            UpdateState::Idle => None,
            UpdateState::Checking => {
                Some(cell("checking for updates…".into(), theme.t3).into_any_element())
            }
            UpdateState::Latest => {
                Some(cell("xenon is up to date".into(), theme.t3).into_any_element())
            }
            UpdateState::Failed(message) => {
                Some(cell(message.clone(), theme.err).into_any_element())
            }
            UpdateState::Available(release) => {
                let url = release.url.clone();
                Some(
                    cell(
                        format!("xenon {} is out ↗", release.version).into(),
                        theme.acc,
                    )
                    .id("update")
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.s3))
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .into_any_element(),
                )
            }
        };
        bar.when(open <= 1, |bar| bar.child(div().flex_1()))
            .children(totals)
            .children(update.map(|cell| {
                div()
                    .h_full()
                    .border_l_1()
                    .border_color(theme.line)
                    .child(cell)
            }))
            .child(
                cell(
                    concat!("xenon ", env!("CARGO_PKG_VERSION")).into(),
                    theme.t3,
                )
                .border_l_1(),
            )
    }
}

/// The workspace that decides whether Xenon may quit (it asks when a
/// transaction is open), for Quit handled outside it.
pub(crate) struct QuitGuard(pub gpui::WeakEntity<Workspace>);

impl gpui::Global for QuitGuard {}

/// Why a production query stopped early.
fn time_limit_note(limit: Option<std::time::Duration>) -> String {
    let seconds = limit.map_or(0, |l| l.as_secs());
    let shown = if seconds.is_multiple_of(60) {
        format!("{} min", seconds / 60)
    } else {
        format!("{seconds} s")
    };
    format!(
        "Stopped after {shown}: the time limit for queries on production. \
         Change it in Settings → Safety."
    )
}

const ATOMIC_ROLLED_BACK: &str =
    "The script ran as one transaction (production), so nothing it did was kept";

/// "1 change", "3 changes".
fn changes(n: usize) -> String {
    if n == 1 {
        "1 change".into()
    } else {
        format!("{n} changes")
    }
}

/// How a production statement is wrapped in a transaction.
#[derive(Clone, Copy)]
enum Wrap {
    /// A script that writes: all of it or none.
    Atomic(&'static str),
    /// What looks like a read: in a transaction the server keeps read-only.
    ReadOnly(&'static str),
}

impl Wrap {
    fn begin(self) -> &'static str {
        match self {
            Wrap::Atomic(begin) | Wrap::ReadOnly(begin) => begin,
        }
    }

    /// The error, with what the wrapping did about it.
    fn explain(self, err: anyhow::Error) -> anyhow::Error {
        match self {
            Wrap::Atomic(_) => err.context(ATOMIC_ROLLED_BACK),
            Wrap::ReadOnly(_) if refused_in_read_only(&err) => err.context(
                "Production reads run in a read-only transaction, and this one tried to write, so \
                 the server refused it. To run it anyway, wrap it in begin … commit",
            ),
            Wrap::ReadOnly(_) => err,
        }
    }
}

/// The server's "you can't write in a read-only transaction" (Postgres 25006,
/// MySQL 1792).
fn refused_in_read_only(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_ascii_lowercase();
    message.contains("read-only transaction") || message.contains("read only transaction")
}

/// The panel floating on the frosted window that holds the editor, results and inspector.
fn card(theme: &Theme, classic: bool) -> gpui::Div {
    if classic {
        return div().bg(theme.s1);
    }
    div()
        .bg(theme.s1)
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(theme.card_line)
        .overflow_hidden()
        .shadow_sm()
}

/// Empty title bar space, which drags the window.
fn drag_area(cx: &mut Context<Workspace>) -> gpui::Div {
    div().h_full().on_mouse_down(
        MouseButton::Left,
        cx.listener(|this, _, _, _| this.titlebar_pressed = true),
    )
}

/// The window behind the frame: blurred desktop, or solid.
fn window_material(frosted: bool) -> gpui::WindowBackgroundAppearance {
    if frosted {
        gpui::WindowBackgroundAppearance::Blurred
    } else {
        gpui::WindowBackgroundAppearance::Opaque
    }
}

/// A small icon button for the results bar.
pub(crate) fn icon_button(
    theme: &Theme,
    id: impl Into<gpui::ElementId>,
    icon: &'static str,
    active: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(24.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .cursor_pointer()
        .when(active, |el| el.bg(theme.s3))
        .hover(|el| el.bg(theme.s3))
        .child(
            svg()
                .path(assets::icon(icon))
                .size(px(14.))
                .text_color(if active { theme.text } else { theme.t2 }),
        )
}

/// How far open a sliding panel is, from 0 to 1: eased, over
/// [`SIDE_PANEL_SLIDE`] from `since`, towards `open`.
fn slide(since: Option<std::time::Instant>, open: bool, window: &Window) -> f32 {
    let progress = since.map_or(1., |since| {
        (since.elapsed().as_secs_f32() / SIDE_PANEL_SLIDE).min(1.)
    });
    if progress < 1. {
        window.request_animation_frame();
    }
    let eased = 1. - (1. - progress).powi(3);
    if open { eased } else { 1. - eased }
}
