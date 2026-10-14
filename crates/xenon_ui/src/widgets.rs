//! Small Steel-styled building blocks shared by the panels.

use std::time::Duration;

use gpui::{AnyElement, FontWeight, IntoElement, Rgba, div, prelude::*, px};

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
    keycaps(
        keys,
        theme.bg,
        gpui::rgba(0x00000026),
        gpui::rgba(0x00000033),
    )
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

/// A search field: icon, the input (with a clear button) and an accent ring
/// while focused.
pub fn search_field(
    theme: &Theme,
    input: &gpui::Entity<gpui_component::input::InputState>,
    focused: bool,
) -> gpui::Div {
    field(
        theme,
        gpui::svg()
            .path(crate::assets::icon("search"))
            .size(px(14.))
            .flex_none()
            .text_color(if focused { theme.acc } else { theme.t3 })
            .into_any_element(),
        input,
        focused,
    )
}

/// A text field with something leading it (an icon, a label).
pub fn field(
    theme: &Theme,
    leading: AnyElement,
    input: &gpui::Entity<gpui_component::input::InputState>,
    focused: bool,
) -> gpui::Div {
    div()
        .h(px(30.))
        .pl(px(9.))
        .pr(px(4.))
        .flex()
        .items_center()
        .gap(px(7.))
        .rounded(px(6.))
        .bg(theme.s2)
        .border_1()
        .border_color(if focused { theme.acc } else { theme.line })
        .child(leading)
        .child(
            div().flex_1().min_w(px(0.)).child(
                gpui_component::input::Input::new(input)
                    .appearance(false)
                    .cleanable(true),
            ),
        )
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
        .px(px(4.))
        .border_1()
        .border_color(color)
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

pub fn panel_button(
    theme: &Theme,
    id: &'static str,
    label: &'static str,
    keys: &'static str,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .mx(px(12.))
        .mt(px(4.))
        .px(px(10.))
        .py(px(6.))
        .flex()
        .items_center()
        .justify_between()
        .border_1()
        .border_color(theme.line)
        .text_color(theme.t2)
        .cursor_pointer()
        .hover(|s| s.text_color(theme.text).border_color(theme.t3))
        .child(label)
        .child(kbd(theme, keys))
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
