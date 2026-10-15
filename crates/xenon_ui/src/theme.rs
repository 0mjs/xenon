//! Colour tokens and accents. Source of truth: PLAN.md, "Design direction".

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

/// The colour for focus and the main action. Picked in Settings; Xenon by default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Accent {
    Steel,
    Graphite,
    #[default]
    Xenon,
    Amber,
    Teal,
    Ember,
}

/// An accent's colours in one mode, plus any status colour it would clash with,
/// moved out of its way.
struct AccentColors {
    acc: u32,
    on_acc: u32,
    hover: u32,
    pressed: u32,
    ok: Option<u32>,
    warn: Option<u32>,
    err: Option<u32>,
}

impl Accent {
    pub const ALL: [Accent; 6] = [
        Accent::Xenon,
        Accent::Graphite,
        Accent::Steel,
        Accent::Amber,
        Accent::Teal,
        Accent::Ember,
    ];
    pub const SETTING: &str = "accent";

    pub fn as_str(self) -> &'static str {
        match self {
            Accent::Steel => "steel",
            Accent::Graphite => "graphite",
            Accent::Xenon => "xenon",
            Accent::Amber => "amber",
            Accent::Teal => "teal",
            Accent::Ember => "ember",
        }
    }

    pub fn parse(value: &str) -> Self {
        Accent::ALL
            .into_iter()
            .find(|a| a.as_str() == value)
            .unwrap_or_default()
    }

    pub fn label(self) -> &'static str {
        match self {
            Accent::Steel => "Steel",
            Accent::Graphite => "Graphite",
            Accent::Xenon => "Xenon",
            Accent::Amber => "Amber",
            Accent::Teal => "Teal",
            Accent::Ember => "Ember",
        }
    }

    /// The swatch shown in Settings.
    pub fn swatch(self, mode: Mode) -> Rgba {
        rgb(self.colors(mode).acc)
    }

    fn colors(self, mode: Mode) -> AccentColors {
        let plain = |acc, on_acc, hover, pressed| AccentColors {
            acc,
            on_acc,
            hover,
            pressed,
            ok: None,
            warn: None,
            err: None,
        };
        match (self, mode) {
            (Accent::Xenon, Mode::Dark) => plain(0xb9c7dc, 0x0b0c0e, 0xcdd8e8, 0xa5b4cb),
            (Accent::Xenon, Mode::Light) => plain(0x4b5d78, 0xffffff, 0x42536b, 0x3a495e),
            (Accent::Graphite, Mode::Dark) => plain(0xe6e7ea, 0x0b0c0e, 0xf4f5f7, 0xcfd1d6),
            (Accent::Graphite, Mode::Light) => plain(0x2b2e35, 0xffffff, 0x3a3e46, 0x1f2126),
            (Accent::Steel, Mode::Dark) => plain(0x7ba7e8, 0x06080c, 0x93b8ee, 0x6a97da),
            (Accent::Steel, Mode::Light) => plain(0x2f64c0, 0xffffff, 0x2a59ab, 0x244e97),
            // Amber sits on the number colour, so numbers turn lilac.
            (Accent::Amber, Mode::Dark) => AccentColors {
                warn: Some(0xc9b3e0),
                ..plain(0xe6a756, 0x140c02, 0xeeb877, 0xd39440)
            },
            (Accent::Amber, Mode::Light) => AccentColors {
                warn: Some(0x7a4fb0),
                ..plain(0xa8641a, 0xffffff, 0x965815, 0x844d11)
            },
            // Teal sits near the string green, so strings go yellow-green.
            (Accent::Teal, Mode::Dark) => AccentColors {
                ok: Some(0xb6d68c),
                ..plain(0x4fbfae, 0x03100d, 0x6dcdbd, 0x3faa9a)
            },
            (Accent::Teal, Mode::Light) => AccentColors {
                ok: Some(0x5f7f2e),
                ..plain(0x1f7f72, 0xffffff, 0x1b7065, 0x176258)
            },
            // Ember sits near production red, so that red deepens.
            (Accent::Ember, Mode::Dark) => AccentColors {
                err: Some(0xff5d6c),
                ..plain(0xee7f5c, 0x160803, 0xf29676, 0xdc6c49)
            },
            (Accent::Ember, Mode::Light) => AccentColors {
                err: Some(0xc4283a),
                ..plain(0xc2502d, 0xffffff, 0xad4626, 0x983d21)
            },
        }
    }
}

/// The accent in use, kept so a change of light/dark keeps it.
struct CurrentAccent(Accent);
impl Global for CurrentAccent {}

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
        Self::new(mode, Accent::default())
    }

    pub fn new(mode: Mode, accent: Accent) -> Self {
        let mut theme = match mode {
            Mode::Dark => Self::dark(),
            Mode::Light => Self::light(),
        };
        let colors = accent.colors(mode);
        theme.acc = rgb(colors.acc);
        theme.acc_wash = Rgba {
            a: 0.12,
            ..rgb(colors.acc)
        };
        theme.on_acc = rgb(colors.on_acc);
        if let Some(ok) = colors.ok {
            theme.ok = rgb(ok);
        }
        if let Some(warn) = colors.warn {
            theme.warn = rgb(warn);
        }
        if let Some(err) = colors.err {
            theme.err = rgb(err);
        }
        theme
    }

    /// Near-black chrome, so panels separate by a shade, and the Xenon silver
    /// accent for focus and the main action.
    pub fn dark() -> Self {
        Self {
            mode: Mode::Dark,
            bg: rgb(0x060607),
            s1: rgb(0x0b0c0e),
            s2: rgb(0x101114),
            s3: rgb(0x17191d),
            line: rgb(0x23252b),
            line_2: rgb(0x16171b),
            text: rgb(0xeceef2),
            t2: rgb(0xa2a6af),
            t3: rgb(0x686c76),
            // Xenon: the cold silver-white of a xenon lamp.
            acc: rgb(0xb9c7dc),
            acc_wash: rgba(0xb9c7dc1f),
            ok: rgb(0x8cc49c),
            warn: rgb(0xd8b06a),
            err: rgb(0xe0837a),
            on_acc: rgb(0x0b0c0e),
            overlay: rgb(0x111215),
            field: rgb(0x08090a),
            raised: rgb(0x22252b),
            scrim: rgba(0x020203b3),
            shadow: rgba(0x00000099),
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
            acc: rgb(0x4b5d78),
            acc_wash: rgba(0x4b5d781f),
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

/// The editor's themes (gpui-component), with Xenon's accent in them; other
/// accents are swapped in by [`apply`].
const EDITOR_THEMES: &str = include_str!("../../../assets/themes/steel.json");

/// Register the editor themes with gpui-component and apply the one matching
/// the system appearance.
pub fn install(cx: &mut App) {
    cx.set_global(CurrentAccent(Accent::default()));
    apply(Mode::from_appearance(cx.window_appearance()), cx);
}

/// Switch to `accent`, keeping the current light or dark mode.
pub fn set_accent(accent: Accent, cx: &mut App) {
    cx.set_global(CurrentAccent(accent));
    let mode = cx.global::<Theme>().mode;
    apply(mode, cx);
}

/// Switch both Xenon's tokens and the editor's theme to `mode`, in the current accent.
pub fn apply(mode: Mode, cx: &mut App) {
    let accent = cx
        .try_global::<CurrentAccent>()
        .map_or(Accent::default(), |a| a.0);
    cx.set_global(Theme::new(mode, accent));

    // The bundled JSON carries Xenon's colours; swap in this accent's.
    let mut json = EDITOR_THEMES.to_string();
    for mode in [Mode::Dark, Mode::Light] {
        let (from, to) = (Accent::Xenon.colors(mode), accent.colors(mode));
        let base = Theme::new(mode, Accent::Xenon);
        let hex = |c: u32| format!("#{c:06x}");
        let mut swaps = vec![
            (hex(from.acc), hex(to.acc)),
            (hex(from.hover), hex(to.hover)),
            (hex(from.pressed), hex(to.pressed)),
        ];
        let status = |c: Rgba| {
            ((c.r * 255.).round() as u32) << 16
                | ((c.g * 255.).round() as u32) << 8
                | (c.b * 255.).round() as u32
        };
        for (old, new) in [(base.ok, to.ok), (base.warn, to.warn), (base.err, to.err)] {
            if let Some(new) = new {
                swaps.push((hex(status(old)), hex(new)));
            }
        }
        // Only touch this mode's half of the file.
        let marker = match mode {
            Mode::Dark => "\"name\": \"Xenon Dark\"",
            Mode::Light => "\"name\": \"Xenon Light\"",
        };
        if let Some(start) = json.find(marker) {
            let end = json[start + 1..]
                .find("\"name\": \"Xenon ")
                .map_or(json.len(), |i| start + 1 + i);
            let mut part = json[start..end].to_string();
            for (old, new) in &swaps {
                part = part.replace(old.as_str(), new.as_str());
            }
            json.replace_range(start..end, &part);
        }
    }
    let registry = gpui_component::ThemeRegistry::global_mut(cx);
    registry
        .load_themes_from_str(&json)
        .expect("bundled steel.json is valid");
    let dark = registry.themes()["Xenon Dark"].clone();
    let light = registry.themes()["Xenon Light"].clone();
    let component_theme = gpui_component::Theme::global_mut(cx);
    component_theme.dark_theme = dark;
    component_theme.light_theme = light;

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
