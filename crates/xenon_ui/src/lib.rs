pub mod assets;
mod bench;
mod completion;
mod confirm;
mod connect;
mod detect;
mod discover;
mod edits;
mod explorer;
pub mod fonts;
mod history;
mod launcher;
mod palette;
mod results;
mod servers;
mod settings;
pub mod theme;
mod widgets;
pub mod workspace;

use gpui::{App, KeyBinding, actions};

pub use workspace::{Launch, Workspace};

actions!(
    xenon,
    [
        Quit,
        OpenFile,
        NewConnection,
        ShowConnections,
        OpenSettings,
        EditCell,
        DeleteRows,
        AddRow,
        SaveEdits,
        DiscardEdits,
        CloseConnection,
        RunQuery,
        RunAll,
        Explain,
        CancelQuery,
        NewTab,
        CloseTab,
        NextTab,
        PrevTab,
        ShowExplorer,
        ShowHistory,
        ToggleExplorer,
        ToggleInspector,
        MoveUp,
        MoveDown,
        MoveLeft,
        MoveRight,
        ExtendUp,
        ExtendDown,
        ExtendLeft,
        ExtendRight,
        SelectAllCells,
        CopySelection,
        CopyCsv,
        CopyJson,
        FocusEditor,
        OpenPalette,
        PaletteUp,
        PaletteDown,
        UseSystemTheme,
        UseLightTheme,
        UseDarkTheme
    ]
);

/// Key context of the focused results grid.
const GRID_CONTEXT: &str = "ResultsGrid";

/// The editor's key context. Bindings here must be registered after
/// `gpui_component::init` so they win over the editor's own (`cmd-enter`
/// would otherwise insert a newline).
const EDITOR_CONTEXT: &str = "Input";

/// Opt-in timing log for performance work: `XENON_TRACE=1 xenon`.
pub fn trace(label: &str, since: std::time::Instant) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ENABLED.get_or_init(|| std::env::var_os("XENON_TRACE").is_some()) {
        eprintln!(
            "xenon: {label}: {:.1} ms",
            since.elapsed().as_secs_f64() * 1000.
        );
    }
}

/// When the process started, for startup timing.
pub static LAUNCHED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

pub fn init(cx: &mut App) {
    gpui_component::init(cx);
    theme::install(cx);

    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-o", OpenFile, None),
        KeyBinding::new("cmd-n", NewConnection, None),
        KeyBinding::new("cmd-shift-n", ShowConnections, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-s", SaveEdits, None),
        // Autocomplete opens as you type; ⌃Space opens it on demand.
        KeyBinding::new(
            "ctrl-space",
            gpui_component::input::ShowCompletions,
            Some("Input"),
        ),
        KeyBinding::new("enter", EditCell, Some(GRID_CONTEXT)),
        KeyBinding::new("cmd-backspace", DeleteRows, Some(GRID_CONTEXT)),
        KeyBinding::new(
            "escape",
            gpui_component::input::Escape,
            Some("SettingsDialog"),
        ),
        KeyBinding::new(
            "escape",
            gpui_component::input::Escape,
            Some("ConfirmDialog"),
        ),
        KeyBinding::new("cmd-shift-w", CloseConnection, None),
        KeyBinding::new("cmd-t", NewTab, None),
        KeyBinding::new("cmd-w", CloseTab, None),
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PrevTab, None),
        KeyBinding::new("cmd-shift-]", NextTab, None),
        KeyBinding::new("cmd-shift-[", PrevTab, None),
        KeyBinding::new("cmd-shift-e", ShowExplorer, None),
        KeyBinding::new("cmd-y", ShowHistory, None),
        KeyBinding::new("cmd-enter", RunQuery, Some("Workspace")),
        KeyBinding::new("cmd-enter", RunQuery, Some(EDITOR_CONTEXT)),
        KeyBinding::new("cmd-shift-enter", RunAll, Some("Workspace")),
        KeyBinding::new("cmd-shift-enter", RunAll, Some(EDITOR_CONTEXT)),
        KeyBinding::new("cmd-.", CancelQuery, Some("Workspace")),
        KeyBinding::new("cmd-.", CancelQuery, Some(EDITOR_CONTEXT)),
        KeyBinding::new("cmd-b", ToggleExplorer, Some("Workspace")),
        KeyBinding::new("cmd-i", ToggleInspector, Some("Workspace")),
        KeyBinding::new("cmd-e", Explain, Some("Workspace")),
        KeyBinding::new("cmd-e", Explain, Some(EDITOR_CONTEXT)),
        KeyBinding::new("up", MoveUp, Some(GRID_CONTEXT)),
        KeyBinding::new("down", MoveDown, Some(GRID_CONTEXT)),
        KeyBinding::new("left", MoveLeft, Some(GRID_CONTEXT)),
        KeyBinding::new("right", MoveRight, Some(GRID_CONTEXT)),
        KeyBinding::new("shift-up", ExtendUp, Some(GRID_CONTEXT)),
        KeyBinding::new("shift-down", ExtendDown, Some(GRID_CONTEXT)),
        KeyBinding::new("shift-left", ExtendLeft, Some(GRID_CONTEXT)),
        KeyBinding::new("shift-right", ExtendRight, Some(GRID_CONTEXT)),
        KeyBinding::new("cmd-a", SelectAllCells, Some(GRID_CONTEXT)),
        KeyBinding::new("cmd-c", CopySelection, Some(GRID_CONTEXT)),
        KeyBinding::new("cmd-shift-c", CopyCsv, Some(GRID_CONTEXT)),
        KeyBinding::new("cmd-alt-c", CopyJson, Some(GRID_CONTEXT)),
        KeyBinding::new("escape", FocusEditor, Some(GRID_CONTEXT)),
        KeyBinding::new("cmd-k", OpenPalette, None),
        KeyBinding::new("cmd-p", OpenPalette, None),
        KeyBinding::new("up", PaletteUp, Some("Palette > Input")),
        KeyBinding::new("down", PaletteDown, Some("Palette > Input")),
        KeyBinding::new("ctrl-p", PaletteUp, Some("Palette > Input")),
        KeyBinding::new("ctrl-n", PaletteDown, Some("Palette > Input")),
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());
}

#[cfg(test)]
mod snapshots;
