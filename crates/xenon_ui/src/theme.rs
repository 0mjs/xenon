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
}

impl Global for Theme {}

impl Theme {
    pub fn for_mode(mode: Mode) -> Self {
        match mode {
            Mode::Dark => Self::dark(),
            Mode::Light => Self::light(),
        }
    }

    pub fn dark() -> Self {
        Self {
            mode: Mode::Dark,
            bg: rgb(0x0f1012),
            s1: rgb(0x141518),
            s2: rgb(0x18191d),
            s3: rgb(0x1e2024),
            line: rgb(0x2a2c31),
            line_2: rgb(0x202226),
            text: rgb(0xe4e5e8),
            t2: rgb(0x9b9ea6),
            t3: rgb(0x646770),
            acc: rgb(0x8fa3bd),
            acc_wash: rgba(0x8fa3bd1c),
            ok: rgb(0x94b39d),
            warn: rgb(0xc9a96e),
            err: rgb(0xd0817a),
        }
    }

    pub fn light() -> Self {
        Self {
            mode: Mode::Light,
            bg: rgb(0xeeeff1),
            s1: rgb(0xf6f6f7),
            s2: rgb(0xfbfbfc),
            s3: rgb(0xe9eaec),
            line: rgb(0xd6d8dc),
            line_2: rgb(0xe4e5e8),
            text: rgb(0x17181b),
            t2: rgb(0x5a5e66),
            t3: rgb(0x93969d),
            acc: rgb(0x4b6481),
            acc_wash: rgba(0x4b64811a),
            ok: rgb(0x4d7a5a),
            warn: rgb(0x8a6822),
            err: rgb(0xa8473f),
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
