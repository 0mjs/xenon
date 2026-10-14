//! Carbon / Steel colour tokens. Source of truth: PLAN.md, "Design direction".

use gpui::{App, Global, Rgba, WindowAppearance, rgb, rgba};

pub const UI_FONT: &str = "Archivo";
pub const MONO_FONT: &str = "IBM Plex Mono";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Dark,
    Light,
}

impl Mode {
    pub fn from_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Mode::Dark,
            WindowAppearance::Light | WindowAppearance::VibrantLight => Mode::Light,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub mode: Mode,
    /// Title bar, rail, status bar.
    pub bg: Rgba,
    /// Panels.
    pub s1: Rgba,
    /// Editor, inputs.
    pub s2: Rgba,
    /// Hover, current line.
    pub s3: Rgba,
    /// Panel borders.
    pub line: Rgba,
    /// Grid lines.
    pub line_2: Rgba,
    pub text: Rgba,
    pub t2: Rgba,
    /// Labels, line numbers, NULL.
    pub t3: Rgba,
    /// Focus only: caret, active tab, selection.
    pub acc: Rgba,
    pub acc_wash: Rgba,
    pub ok: Rgba,
    pub warn: Rgba,
    pub err: Rgba,
    /// Text and icons on an accent or danger fill.
    pub on_acc: Rgba,
    /// Dialogs, the palette and other things floating above the window.
    pub overlay: Rgba,
    /// Text field fill: slightly inset from the surface it sits on.
    pub field: Rgba,
    /// The chosen segment of a segmented control.
    pub raised: Rgba,
    /// Dims the window behind a dialog.
    pub scrim: Rgba,
    /// Drop shadow under floating surfaces.
    pub shadow: Rgba,
}

impl Global for Theme {}

impl Theme {
    pub fn for_mode(mode: Mode) -> Self {
        match mode {
            Mode::Dark => Self::dark(),
            Mode::Light => Self::light(),
        }
    }

    /// Darker chrome, brighter text and a clearer steel-blue accent than the
    /// first cut, so panels separate and focus reads at a glance.
    pub fn dark() -> Self {
        Self {
            mode: Mode::Dark,
            bg: rgb(0x0b0c0e),
            s1: rgb(0x121317),
            s2: rgb(0x17191d),
            s3: rgb(0x20232a),
            line: rgb(0x2b2e36),
            line_2: rgb(0x1d1f24),
            text: rgb(0xeceef2),
            t2: rgb(0xa4a9b3),
            t3: rgb(0x6c717c),
            acc: rgb(0x7ba7e8),
            acc_wash: rgba(0x7ba7e826),
            ok: rgb(0x8cc49c),
            warn: rgb(0xd8b06a),
            err: rgb(0xe0837a),
            on_acc: rgb(0x0b0c0e),
            overlay: rgb(0x16181c),
            field: rgb(0x0f1013),
            raised: rgb(0x2a2d35),
            scrim: rgba(0x05060799),
            shadow: rgba(0x00000080),
        }
    }

    pub fn light() -> Self {
        Self {
            mode: Mode::Light,
            bg: rgb(0xe6e8ec),
            s1: rgb(0xf4f5f7),
            s2: rgb(0xffffff),
            s3: rgb(0xe3e6eb),
            line: rgb(0xcfd3da),
            line_2: rgb(0xe4e7eb),
            text: rgb(0x111318),
            t2: rgb(0x535a66),
            t3: rgb(0x8c929d),
            acc: rgb(0x2f64c0),
            acc_wash: rgba(0x2f64c01f),
            ok: rgb(0x3f7d50),
            warn: rgb(0x8f6516),
            err: rgb(0xb2402f),
            on_acc: rgb(0xffffff),
            overlay: rgb(0xffffff),
            field: rgb(0xffffff),
            raised: rgb(0xffffff),
            scrim: rgba(0x1113182e),
            shadow: rgba(0x1113182e),
        }
    }
}

/// Register the Steel themes with gpui-component (which styles the SQL editor)
/// and apply the one matching the system appearance.
pub fn install(cx: &mut App) {
    let registry = gpui_component::ThemeRegistry::global_mut(cx);
    registry
        .load_themes_from_str(include_str!("../../../assets/themes/steel.json"))
        .expect("bundled steel.json is valid");
    let dark = registry.themes()["Steel Dark"].clone();
    let light = registry.themes()["Steel Light"].clone();

    let component_theme = gpui_component::Theme::global_mut(cx);
    component_theme.dark_theme = dark;
    component_theme.light_theme = light;

    apply(Mode::from_appearance(cx.window_appearance()), cx);
}

/// Switch both Xenon's tokens and gpui-component's theme to `mode`.
pub fn apply(mode: Mode, cx: &mut App) {
    cx.set_global(Theme::for_mode(mode));
    let component_mode = match mode {
        Mode::Dark => gpui_component::ThemeMode::Dark,
        Mode::Light => gpui_component::ThemeMode::Light,
    };
    gpui_component::Theme::change(component_mode, None, cx);
}

pub trait ActiveTheme {
    fn theme(&self) -> &Theme;
}

impl ActiveTheme for App {
    fn theme(&self) -> &Theme {
        self.global::<Theme>()
    }
}
