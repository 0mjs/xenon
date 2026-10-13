mod connect;
pub mod fonts;
mod results;
pub mod theme;
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
        CancelQuery,
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
        KeyBinding::new("cmd-enter", RunQuery, Some("Workspace")),
        KeyBinding::new("cmd-enter", RunQuery, Some(EDITOR_CONTEXT)),
        KeyBinding::new("cmd-.", CancelQuery, Some("Workspace")),
        KeyBinding::new("cmd-.", CancelQuery, Some(EDITOR_CONTEXT)),
        KeyBinding::new("cmd-b", ToggleExplorer, Some("Workspace")),
        KeyBinding::new("cmd-i", ToggleInspector, Some("Workspace")),
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());
}
