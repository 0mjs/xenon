mod connect;
mod explorer;
pub mod fonts;
mod history;
mod results;
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
        RunQuery,
        RunAll,
        CancelQuery,
        NewTab,
        CloseTab,
        NextTab,
        PrevTab,
        ShowExplorer,
        ShowHistory,
        ToggleExplorer,
        ToggleInspector
    ]
);

/// The editor's key context. Bindings here must be registered after
/// `gpui_component::init` so they win over the editor's own (`cmd-enter`
/// would otherwise insert a newline).
const EDITOR_CONTEXT: &str = "Input";

pub fn init(cx: &mut App) {
    gpui_component::init(cx);
    theme::install(cx);

    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-o", OpenFile, None),
        KeyBinding::new("cmd-n", NewConnection, None),
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
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());
}
