pub mod fonts;
pub mod theme;
pub mod workspace;

use gpui::{App, KeyBinding, actions};

pub use workspace::Workspace;

actions!(xenon, [Quit, ToggleExplorer, ToggleInspector]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-b", ToggleExplorer, Some("Workspace")),
        KeyBinding::new("cmd-i", ToggleInspector, Some("Workspace")),
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());
}
