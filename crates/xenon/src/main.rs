use std::path::PathBuf;

use gpui::{
    App, Bounds, Menu, MenuItem, SystemMenuType, TitlebarOptions, WindowBounds, WindowOptions,
    point, prelude::*, px, size,
};
use gpui_component::Root;
use gpui_platform::application;
use xenon_ui::{OpenFile, Quit, ToggleExplorer, ToggleInspector, Workspace};

fn main() {
    // `xenon path/to/file.db` opens that database on launch.
    let path = std::env::args_os().nth(1).map(PathBuf::from);

    application().run(|cx: &mut App| {
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
                ..Default::default()
            },
            |window, cx| {
                window.set_rem_size(px(16.));
                let workspace = cx.new(|cx| Workspace::new(path, window, cx));
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
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Quit Xenon", Quit),
        ]),
        Menu::new("File").items([MenuItem::action("Open…", OpenFile)]),
        Menu::new("View").items([
            MenuItem::action("Toggle Explorer", ToggleExplorer),
            MenuItem::action("Toggle Inspector", ToggleInspector),
        ]),
    ]);
}
