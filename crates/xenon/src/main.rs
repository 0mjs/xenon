use gpui::{
    App, Bounds, Menu, MenuItem, SystemMenuType, TitlebarOptions, WindowBounds, WindowOptions,
    point, prelude::*, px, size,
};
use gpui_component::Root;
use gpui_platform::application;
use xenon_ui::{
    CancelQuery, CloseConnection, CloseTab, CopyCsv, CopyJson, CopySelection, Explain, Launch,
    NewConnection, NewTab, NextTab, OpenFile, OpenPalette, OpenSettings, PrevTab, Quit, RunAll,
    RunQuery, ShowConnections, ShowExplorer, ShowHistory, ToggleExplorer, ToggleInspector,
    UseDarkTheme, UseLightTheme, UseSystemTheme, Workspace,
};

fn main() {
    xenon_ui::LAUNCHED.get_or_init(std::time::Instant::now);
    // `xenon path/to/file.db` or `xenon postgres://…` opens that database on launch.
    let launch = std::env::args().nth(1).map(Launch::from_arg);

    application()
        .with_assets(xenon_ui::assets::Assets)
        .run(|cx: &mut App| {
            if let Err(err) = xenon_ui::fonts::load(cx) {
                eprintln!("xenon: failed to load bundled fonts: {err:#}");
            }
            xenon_ui::init(cx);
            set_menus(cx);

            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Xenon".into()),
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(12.), px(11.))),
                    }),
                    window_min_size: Some(size(px(720.), px(480.))),
                    // Frosted glass: the desktop shows, blurred, through the frame.
                    window_background: gpui::WindowBackgroundAppearance::Blurred,
                    ..Default::default()
                },
                |window, cx| {
                    window.set_rem_size(px(16.));
                    // Decoding the icon takes tens of milliseconds; do it after the first frame.
                    #[cfg(target_os = "macos")]
                    window.on_next_frame(|_, _| set_dock_icon());
                    let workspace = cx.new(|cx| Workspace::new(launch, window, cx));
                    cx.new(|cx| Root::new(workspace, window, cx))
                },
            )
            .expect("failed to open the main window");

            cx.activate(true);
        });
}

fn set_menus(cx: &mut App) {
    cx.set_menus([
        Menu::new("Xenon").items([
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Quit Xenon", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Query Tab", NewTab),
            MenuItem::action("Close Tab", CloseTab),
            MenuItem::separator(),
            MenuItem::action("Connections…", ShowConnections),
            MenuItem::action("Open SQLite File…", OpenFile),
            MenuItem::action("New Connection…", NewConnection),
            MenuItem::action("Disconnect", CloseConnection),
        ]),
        Menu::new("Query").items([
            MenuItem::action("Run Statement", RunQuery),
            MenuItem::action("Run All", RunAll),
            MenuItem::action("Explain", Explain),
            MenuItem::action("Cancel", CancelQuery),
            MenuItem::separator(),
            MenuItem::action("Copy Cells", CopySelection),
            MenuItem::action("Copy as CSV", CopyCsv),
            MenuItem::action("Copy as JSON", CopyJson),
        ]),
        Menu::new("View").items([
            MenuItem::action("Command Palette…", OpenPalette),
            MenuItem::separator(),
            MenuItem::action("Explorer", ShowExplorer),
            MenuItem::action("History", ShowHistory),
            MenuItem::separator(),
            MenuItem::action("Toggle Side Panel", ToggleExplorer),
            MenuItem::action("Toggle Inspector", ToggleInspector),
            MenuItem::separator(),
            MenuItem::action("Next Tab", NextTab),
            MenuItem::action("Previous Tab", PrevTab),
            MenuItem::separator(),
            MenuItem::submenu(Menu::new("Theme").items([
                MenuItem::action("Match System", UseSystemTheme),
                MenuItem::action("Light", UseLightTheme),
                MenuItem::action("Dark", UseDarkTheme),
            ])),
        ]),
    ]);
}

/// Inside `Xenon.app` macOS takes the icon from the bundle; a bare binary
/// (`cargo run`) would otherwise show the generic "exec" icon in the Dock.
#[cfg(target_os = "macos")]
fn set_dock_icon() {
    use objc2::{AllocAnyThread as _, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    // 512px is plenty for the Dock (and decodes ~4x faster than the 1024px master).
    const ICON: &[u8] = include_bytes!("../../../assets/icon/xenon-512.png");

    let bundled = std::env::current_exe()
        .is_ok_and(|exe| exe.to_string_lossy().contains(".app/Contents/MacOS/"));
    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };
    if bundled {
        return;
    }
    let data = NSData::with_bytes(ICON);
    if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
        let app = NSApplication::sharedApplication(main_thread);
        // SAFETY: called on the main thread (checked above) with a valid NSImage.
        unsafe { app.setApplicationIconImage(Some(&image)) };
    }
}
