//! Small Steel-styled building blocks shared by the panels.

use std::time::Duration;

use gpui::{AnyElement, FontWeight, IntoElement, Rgba, div, prelude::*, px};
use gpui_component::Sizable as _;

use crate::theme::{MONO_FONT, Theme};

pub fn panel_label(theme: &Theme, text: &str) -> AnyElement {
    div()
        .px(px(12.))
        .pt(px(10.))
        .pb(px(6.))
        .font_family(MONO_FONT)
        .text_size(px(10.))
        .text_color(theme.t3)
        .child(text.to_uppercase())
        .into_any_element()
}

pub fn empty_hint(theme: &Theme, text: &'static str) -> AnyElement {
    div()
        .px(px(12.))
        .py(px(6.))
        .text_color(theme.t3)
        .child(text)
        .into_any_element()
}

pub fn centered_hint(theme: &Theme, text: &'static str) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_color(theme.t3)
        .child(text)
        .into_any_element()
}

/// The system font draws ⌘ ⇧ ⌥ ⌃ ↵ crisply and centred; the UI and mono
/// fonts don't have those glyphs, so they'd fall back unevenly.
const KEY_FONT: &str = ".SystemUIFont";

/// A shortcut as keycaps: "⇧⌘↵" draws three caps.
pub fn kbd(theme: &Theme, keys: &str) -> gpui::Div {
    keycaps(keys, theme.t2, theme.s3, theme.line)
}

/// Keycaps drawn on an accent-filled button.
pub fn kbd_on_accent(theme: &Theme, keys: &str) -> gpui::Div {
    let (fill, border) = match theme.mode {
        crate::theme::Mode::Dark => (gpui::rgba(0x0000001f), gpui::rgba(0x0000002e)),
        crate::theme::Mode::Light => (gpui::rgba(0xffffff26), gpui::rgba(0xffffff38)),
    };
    keycaps(keys, theme.on_acc, fill, border)
}

fn keycaps(keys: &str, color: Rgba, background: Rgba, border: Rgba) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(2.))
        .children(split_keys(keys).into_iter().map(move |key| {
            div()
                .h(px(18.))
                .min_w(px(18.))
                .px(px(4.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .bg(background)
                .border_1()
                .border_color(border)
                .font_family(KEY_FONT)
                .text_size(px(11.))
                .line_height(px(14.))
                .text_color(color)
                .child(key)
        }))
}

/// "⇧⌘↵" → ["⇧", "⌘", "↵"]; "⌘Tab" → ["⌘", "Tab"]. Modifiers get a cap each,
/// whatever follows them is one key.
fn split_keys(keys: &str) -> Vec<String> {
    let mut caps = Vec::new();
    let mut rest = String::new();
    for c in keys.chars() {
        if "⌃⌥⇧⌘".contains(c) && rest.is_empty() {
            caps.push(c.to_string());
        } else if !c.is_whitespace() {
            rest.push(c);
        }
    }
    if !rest.is_empty() {
        caps.push(rest);
    }
    caps
}

/// Corner radii: one per tier, so every surface of a kind rounds the same.
pub const RADIUS_CONTROL: f32 = 6.;
pub const RADIUS_CARD: f32 = 8.;
pub const RADIUS_OVERLAY: f32 = 12.;

/// gpui-component sizes input text at 0.875 × the size given; this lands on
/// the UI's 12px instead of its default (which reads as 10.5px here).
fn input_size() -> gpui_component::Size {
    input_size_for(12.)
}

fn input_size_for(text: f32) -> gpui_component::Size {
    gpui_component::Size::Size(px(text / 0.875))
}

/// A soft ring outside a focused control, so focus reads without the border
/// alone having to carry it.
pub fn focus_ring(theme: &Theme) -> Vec<gpui::BoxShadow> {
    vec![gpui::BoxShadow::new(px(0.), px(0.), theme.acc_wash.into()).spread_radius(px(3.))]
}

/// The shadow under dialogs and the palette.
pub fn overlay_shadow(theme: &Theme) -> Vec<gpui::BoxShadow> {
    vec![
        gpui::BoxShadow::new(px(0.), px(24.), theme.shadow.into()).blur_radius(px(48.)),
        gpui::BoxShadow::new(px(0.), px(2.), theme.shadow.into()).blur_radius(px(6.)),
    ]
}

/// A search field: icon, the input (with a clear button) and an accent ring
/// while focused.
pub fn search_field(
    theme: &Theme,
    input: &gpui::Entity<gpui_component::input::InputState>,
    focused: bool,
) -> gpui::Div {
    field(
        theme,
        Some(
            gpui::svg()
                .path(crate::assets::icon("search"))
                .size(px(14.))
                .flex_none()
                .text_color(if focused { theme.acc } else { theme.t3 })
                .into_any_element(),
        ),
        input,
        focused,
    )
}

/// A single-line text field, optionally led by an icon or a label. 30px high,
/// so it lines up with buttons.
pub fn field(
    theme: &Theme,
    leading: Option<AnyElement>,
    input: &gpui::Entity<gpui_component::input::InputState>,
    focused: bool,
) -> gpui::Div {
    field_sized(theme, leading, input, focused, 12.)
}

/// `field` with larger text, for a field that is the point of its screen.
pub fn field_sized(
    theme: &Theme,
    leading: Option<AnyElement>,
    input: &gpui::Entity<gpui_component::input::InputState>,
    focused: bool,
    text: f32,
) -> gpui::Div {
    field_frame(theme, focused)
        .h(px(30.))
        .when_some(leading, |el, leading| el.pl(px(10.)).child(leading))
        .child(
            div().flex_1().min_w(px(0.)).child(
                gpui_component::input::Input::new(input)
                    .with_size(input_size_for(text))
                    .appearance(false)
                    .cleanable(true),
            ),
        )
}

/// A dialog's text field: no clear button, a touch taller.
pub fn text_field(
    theme: &Theme,
    input: &gpui::Entity<gpui_component::input::InputState>,
    focused: bool,
) -> gpui::Div {
    field_frame(theme, focused).h(px(32.)).child(
        div().flex_1().min_w(px(0.)).child(
            gpui_component::input::Input::new(input)
                .with_size(input_size())
                .appearance(false),
        ),
    )
}

fn field_frame(theme: &Theme, focused: bool) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(2.))
        .pr(px(4.))
        .rounded(px(RADIUS_CONTROL))
        .bg(theme.field)
        .border_1()
        .border_color(if focused { theme.acc } else { theme.line })
        .when(focused, |el| el.shadow(focus_ring(theme)))
}

/// Whether `input` has keyboard focus, for drawing its ring.
pub fn is_focused(
    input: &gpui::Entity<gpui_component::input::InputState>,
    window: &gpui::Window,
    cx: &gpui::App,
) -> bool {
    gpui::Focusable::focus_handle(input.read(cx), cx).is_focused(window)
}

/// A label above a dialog field.
pub fn field_label(theme: &Theme, label: &'static str) -> gpui::Div {
    div()
        .text_size(px(11.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.t2)
        .child(label)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    Primary,
    Secondary,
    Danger,
    /// No fill until hovered: for quiet actions next to a primary one.
    Ghost,
}

/// A 30px button. `enabled: false` dims it and drops the pointer cursor; the
/// caller decides whether its click handler still does anything.
pub fn button(
    theme: &Theme,
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
    kind: ButtonKind,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    let hover_fill = match theme.mode {
        crate::theme::Mode::Dark => gpui::rgba(0xffffff1f),
        crate::theme::Mode::Light => gpui::rgba(0xffffff33),
    };
    let base = div()
        .id(id)
        .h(px(30.))
        .px(px(12.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .rounded(px(RADIUS_CONTROL))
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .when(enabled, |el| el.cursor_pointer())
        .when(!enabled, |el| el.opacity(0.45))
        .child(label.into());
    let filled = |el: gpui::Stateful<gpui::Div>, fill: Rgba| {
        el.bg(fill)
            .text_color(theme.on_acc)
            .when(enabled, |el| el.hover(|s| s.bg(blend(fill, hover_fill))))
    };
    match kind {
        ButtonKind::Primary => filled(base, theme.acc),
        ButtonKind::Danger => filled(base, theme.err),
        ButtonKind::Secondary => base
            .bg(theme.s3)
            .border_1()
            .border_color(theme.line)
            .text_color(theme.text)
            .when(enabled, |el| el.hover(|s| s.bg(theme.raised))),
        ButtonKind::Ghost => base.text_color(theme.t2).when(enabled, |el| {
            el.hover(|s| s.bg(theme.s3).text_color(theme.text))
        }),
    }
}

/// `over` composited onto `base`.
fn blend(base: Rgba, over: Rgba) -> Rgba {
    let mix = |b: f32, o: f32| b * (1. - over.a) + o * over.a;
    Rgba {
        r: mix(base.r, over.r),
        g: mix(base.g, over.g),
        b: mix(base.b, over.b),
        a: base.a,
    }
}

/// A row of mutually exclusive options; `on_pick` receives the chosen index.
pub fn segmented(
    theme: &Theme,
    id: &'static str,
    options: Vec<(&'static str, bool)>,
    on_pick: impl Fn(&usize, &mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::Div {
    let on_pick = std::rc::Rc::new(on_pick);
    div()
        .flex()
        .flex_row()
        .p(px(2.))
        .gap(px(2.))
        .rounded(px(RADIUS_CONTROL + 1.))
        .bg(theme.field)
        .border_1()
        .border_color(theme.line)
        .children(
            options
                .into_iter()
                .enumerate()
                .map(|(ix, (label, active))| {
                    let on_pick = on_pick.clone();
                    div()
                        .id((id, ix))
                        .flex_1()
                        .h(px(26.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(RADIUS_CONTROL - 1.))
                        .text_size(px(12.))
                        .cursor_pointer()
                        .when(active, |el| {
                            el.bg(theme.raised)
                                .text_color(theme.text)
                                .font_weight(FontWeight::MEDIUM)
                                .shadow(vec![
                                    gpui::BoxShadow::new(px(0.), px(1.), theme.shadow.into())
                                        .blur_radius(px(2.)),
                                ])
                        })
                        .when(!active, |el| {
                            el.text_color(theme.t2).hover(|s| s.text_color(theme.text))
                        })
                        .on_click(move |_, window, cx| on_pick(&ix, window, cx))
                        .child(label)
                }),
        )
}

/// A 14px checkbox square.
pub fn checkbox(theme: &Theme, checked: bool) -> gpui::Div {
    div()
        .size(px(15.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .border_1()
        .when(checked, |el| {
            el.bg(theme.acc).border_color(theme.acc).child(
                gpui::svg()
                    .path(crate::assets::icon("check"))
                    .size(px(11.))
                    .text_color(theme.on_acc),
            )
        })
        .when(!checked, |el| el.bg(theme.field).border_color(theme.t3))
}

/// The floating card a dialog sits in.
pub fn overlay_panel(theme: &Theme) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .rounded(px(RADIUS_OVERLAY))
        .bg(theme.overlay)
        .border_1()
        .border_color(theme.line)
        .shadow(overlay_shadow(theme))
}

/// Database engines Xenon shows a logo for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Postgres,
    Sqlite,
    MySql,
    MariaDb,
    DuckDb,
    SqlServer,
    Unknown,
}

impl Engine {
    /// From a display name like "Postgres" or "SQL Server".
    pub fn from_name(name: &str) -> Self {
        match name {
            "Postgres" | "PostgreSQL" => Engine::Postgres,
            "SQLite" => Engine::Sqlite,
            "MySQL" => Engine::MySql,
            "MariaDB" => Engine::MariaDb,
            "DuckDB" => Engine::DuckDb,
            "SQL Server" => Engine::SqlServer,
            _ => Engine::Unknown,
        }
    }

    pub fn from_server(engine: xenon_store::ServerEngine) -> Self {
        match engine {
            xenon_store::ServerEngine::Postgres => Engine::Postgres,
            xenon_store::ServerEngine::Mysql => Engine::MySql,
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Engine::Postgres => "db-postgresql",
            Engine::Sqlite => "db-sqlite",
            Engine::MySql => "db-mysql",
            Engine::MariaDb => "db-mariadb",
            Engine::DuckDb => "db-duckdb",
            Engine::SqlServer => "db-microsoftsqlserver",
            Engine::Unknown => "database",
        }
    }
}

/// An engine's logo, one colour.
pub fn engine_logo(engine: Engine, size: f32, color: Rgba) -> gpui::Svg {
    gpui::svg()
        .path(crate::assets::icon(engine.icon()))
        .size(px(size))
        .flex_none()
        .text_color(color)
}

/// An engine's logo on a 34px tile: accent when it can be used, muted when not yet.
pub fn engine_badge(theme: &Theme, engine: Engine, available: bool) -> gpui::Div {
    div()
        .size(px(34.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(RADIUS_CONTROL + 1.))
        .bg(if available { theme.acc_wash } else { theme.s3 })
        .child(engine_logo(
            engine,
            20.,
            if available { theme.acc } else { theme.t3 },
        ))
}

pub fn tree_row(depth: usize) -> gpui::Div {
    div()
        .h(px(24.))
        .pl(px(12. + depth as f32 * 12.))
        .pr(px(12.))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.))
}

pub fn env_tag(label: &'static str, color: Rgba) -> impl IntoElement {
    div()
        .h(px(16.))
        .px(px(5.))
        .flex()
        .flex_none()
        .items_center()
        .rounded(px(3.))
        .bg(Rgba { a: 0.12, ..color })
        .border_1()
        .border_color(Rgba { a: 0.35, ..color })
        .text_color(color)
        .font_family(MONO_FONT)
        .text_size(px(9.))
        .font_weight(FontWeight::NORMAL)
        .child(label)
}

/// The tags shown for a connection: PROD, DEV, READ, LOCAL.
pub fn connection_tags(
    theme: &Theme,
    environment: xenon_store::Environment,
    read_only: bool,
    local: bool,
) -> Vec<(&'static str, Rgba)> {
    use xenon_store::Environment;
    let mut tags = Vec::new();
    match environment {
        Environment::Production => tags.push(("PROD", theme.err)),
        Environment::Development => tags.push(("DEV", theme.t2)),
        Environment::None if local => tags.push(("LOCAL", theme.t3)),
        Environment::None => {}
    }
    if read_only {
        tags.push(("READ", theme.warn));
    }
    tags
}

pub fn format_duration(elapsed: Duration) -> String {
    let ms = elapsed.as_secs_f64() * 1000.;
    if ms < 1. {
        format!("{:.2} ms", ms)
    } else if ms < 1000. {
        format!("{:.1} ms", ms)
    } else {
        format!("{:.2} s", ms / 1000.)
    }
}

pub fn group_digits(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Re-indent JSON text for reading. Returns `None` unless `text` is an object or
/// array with balanced brackets (strings are respected), so ordinary text is
/// never mangled.
pub fn pretty_json(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return None;
    }
    let mut out = String::with_capacity(trimmed.len() * 2);
    let mut depth: usize = 0;
    let mut in_string = false;
    let mut escaped = false;
    let newline = |out: &mut String, depth: usize| {
        out.push('\n');
        out.push_str(&"  ".repeat(depth));
    };
    let mut chars = trimmed.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            match (escaped, ch) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                out.push(ch);
            }
            '{' | '[' => {
                out.push(ch);
                // Keep empty containers on one line.
                if matches!(chars.peek(), Some('}') | Some(']')) {
                    continue;
                }
                depth += 1;
                newline(&mut out, depth);
            }
            '}' | ']' => {
                if !matches!(out.chars().last(), Some('{') | Some('[')) {
                    depth = depth.checked_sub(1)?;
                    newline(&mut out, depth);
                }
                out.push(ch);
            }
            ',' => {
                out.push(ch);
                newline(&mut out, depth);
            }
            ':' => out.push_str(": "),
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
    }
    (depth == 0 && !in_string).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_prints_json() {
        assert_eq!(
            pretty_json(r#"{"theme":"dark","tags":["a","b, c"],"empty":{},"n":1}"#).unwrap(),
            "{\n  \"theme\": \"dark\",\n  \"tags\": [\n    \"a\",\n    \"b, c\"\n  ],\n  \"empty\": {},\n  \"n\": 1\n}"
        );
        assert_eq!(pretty_json(r#"["x\"]"]"#).unwrap(), "[\n  \"x\\\"]\"\n]");
    }

    #[test]
    fn leaves_non_json_alone() {
        assert_eq!(pretty_json("hello"), None);
        assert_eq!(pretty_json("{unbalanced"), None);
        assert_eq!(pretty_json("{\"a\": 1}}"), None);
    }

    #[test]
    fn splits_shortcuts_into_keycaps() {
        assert_eq!(split_keys("⇧⌘↵"), ["⇧", "⌘", "↵"]);
        assert_eq!(split_keys("⌘K"), ["⌘", "K"]);
        assert_eq!(split_keys("⌃⇧Tab"), ["⌃", "⇧", "Tab"]);
        assert_eq!(split_keys("Esc"), ["Esc"]);
        assert_eq!(split_keys("⌘."), ["⌘", "."]);
    }

    #[test]
    fn groups_thousands() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1000), "1,000");
        assert_eq!(group_digits(1234567), "1,234,567");
    }

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration(Duration::from_micros(250)), "0.25 ms");
        assert_eq!(format_duration(Duration::from_micros(38_200)), "38.2 ms");
        assert_eq!(format_duration(Duration::from_millis(2500)), "2.50 s");
    }
}
