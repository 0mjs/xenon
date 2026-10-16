//! Code themes: the syntax colours of popular editor themes, for the SQL
//! editor and anything else highlighted. Xenon's own colours stay around them;
//! each theme has a dark and a light palette, used with the app's mode.

use crate::theme::Mode;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CodeTheme {
    Xenon,
    One,
    #[default]
    GitHub,
    Solarized,
    Catppuccin,
    Gruvbox,
    TokyoNight,
    Dracula,
    Nord,
    Monokai,
}

/// Colours for each kind of token.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub text: u32,
    pub keyword: u32,
    pub function: u32,
    pub string: u32,
    pub number: u32,
    pub comment: u32,
    pub kind: u32,
    pub operator: u32,
    pub property: u32,
    /// Keywords in bold (Xenon's own look).
    pub bold_keywords: bool,
}

/// A palette from its colours: text, keyword, function, string, number,
/// comment, type, operator, property.
const fn palette(c: [u32; 9]) -> Palette {
    Palette {
        text: c[0],
        keyword: c[1],
        function: c[2],
        string: c[3],
        number: c[4],
        comment: c[5],
        kind: c[6],
        operator: c[7],
        property: c[8],
        bold_keywords: false,
    }
}

impl CodeTheme {
    pub const ALL: [CodeTheme; 10] = [
        CodeTheme::Xenon,
        CodeTheme::One,
        CodeTheme::GitHub,
        CodeTheme::Solarized,
        CodeTheme::Catppuccin,
        CodeTheme::Gruvbox,
        CodeTheme::TokyoNight,
        CodeTheme::Dracula,
        CodeTheme::Nord,
        CodeTheme::Monokai,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CodeTheme::Xenon => "Xenon",
            CodeTheme::One => "One",
            CodeTheme::GitHub => "GitHub",
            CodeTheme::Solarized => "Solarized",
            CodeTheme::Catppuccin => "Catppuccin",
            CodeTheme::Gruvbox => "Gruvbox",
            CodeTheme::TokyoNight => "Tokyo Night",
            CodeTheme::Dracula => "Dracula",
            CodeTheme::Nord => "Nord",
            CodeTheme::Monokai => "Monokai",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            CodeTheme::Xenon => "xenon",
            CodeTheme::One => "one",
            CodeTheme::GitHub => "github",
            CodeTheme::Solarized => "solarized",
            CodeTheme::Catppuccin => "catppuccin",
            CodeTheme::Gruvbox => "gruvbox",
            CodeTheme::TokyoNight => "tokyo-night",
            CodeTheme::Dracula => "dracula",
            CodeTheme::Nord => "nord",
            CodeTheme::Monokai => "monokai",
        }
    }

    pub fn parse(value: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|t| t.as_str() == value)
            .unwrap_or_default()
    }

    /// The palette for `mode`: `None` keeps Xenon's own.
    pub fn palette(self, mode: Mode) -> Option<Palette> {
        let dark = mode == Mode::Dark;
        Some(match (self, dark) {
            (CodeTheme::Xenon, _) => return None,
            // One Dark / One Light (Atom).
            (CodeTheme::One, true) => palette([
                0xabb2bf, 0xc678dd, 0x61afef, 0x98c379, 0xd19a66, 0x5c6370, 0xe5c07b, 0x56b6c2,
                0xe06c75,
            ]),
            (CodeTheme::One, false) => palette([
                0x383a42, 0xa626a4, 0x4078f2, 0x50a14f, 0x986801, 0xa0a1a7, 0xc18401, 0x0184bc,
                0xe45649,
            ]),
            // GitHub Dark / Light (Primer).
            (CodeTheme::GitHub, true) => palette([
                0xc9d1d9, 0xff7b72, 0xd2a8ff, 0xa5d6ff, 0x79c0ff, 0x8b949e, 0xffa657, 0xff7b72,
                0x79c0ff,
            ]),
            (CodeTheme::GitHub, false) => palette([
                0x24292f, 0xcf222e, 0x8250df, 0x0a3069, 0x0550ae, 0x6e7781, 0x953800, 0xcf222e,
                0x0550ae,
            ]),
            // Solarized (Ethan Schoonover).
            (CodeTheme::Solarized, true) => palette([
                0x93a1a1, 0x859900, 0x268bd2, 0x2aa198, 0xd33682, 0x586e75, 0xb58900, 0x859900,
                0xcb4b16,
            ]),
            (CodeTheme::Solarized, false) => palette([
                0x586e75, 0x859900, 0x268bd2, 0x2aa198, 0xd33682, 0x93a1a1, 0xb58900, 0x859900,
                0xcb4b16,
            ]),
            // Catppuccin Mocha / Latte.
            (CodeTheme::Catppuccin, true) => palette([
                0xcdd6f4, 0xcba6f7, 0x89b4fa, 0xa6e3a1, 0xfab387, 0x7f849c, 0xf9e2af, 0x89dceb,
                0xb4befe,
            ]),
            (CodeTheme::Catppuccin, false) => palette([
                0x4c4f69, 0x8839ef, 0x1e66f5, 0x40a02b, 0xfe640b, 0x8c8fa1, 0xdf8e1d, 0x04a5e5,
                0x7287fd,
            ]),
            // Gruvbox Dark / Light.
            (CodeTheme::Gruvbox, true) => palette([
                0xebdbb2, 0xfb4934, 0xfabd2f, 0xb8bb26, 0xd3869b, 0x928374, 0xfabd2f, 0xfe8019,
                0x83a598,
            ]),
            (CodeTheme::Gruvbox, false) => palette([
                0x3c3836, 0x9d0006, 0xb57614, 0x79740e, 0x8f3f71, 0x928374, 0xb57614, 0xaf3a03,
                0x076678,
            ]),
            // Tokyo Night / Day.
            (CodeTheme::TokyoNight, true) => palette([
                0xc0caf5, 0xbb9af7, 0x7aa2f7, 0x9ece6a, 0xff9e64, 0x565f89, 0x2ac3de, 0x89ddff,
                0x73daca,
            ]),
            (CodeTheme::TokyoNight, false) => palette([
                0x3760bf, 0x9854f1, 0x2e7de9, 0x587539, 0xb15c00, 0x848cb5, 0x007197, 0x006a83,
                0x387068,
            ]),
            // Dracula / Alucard (its light variant).
            (CodeTheme::Dracula, true) => palette([
                0xf8f8f2, 0xff79c6, 0x50fa7b, 0xf1fa8c, 0xbd93f9, 0x6272a4, 0x8be9fd, 0xff79c6,
                0x66d9ef,
            ]),
            (CodeTheme::Dracula, false) => palette([
                0x1f1f1f, 0xa3144d, 0x14710a, 0x846e15, 0x635d97, 0x6c664b, 0x036a96, 0xa3144d,
                0x036a96,
            ]),
            // Nord; light: its colours deepened to read on white.
            (CodeTheme::Nord, true) => palette([
                0xd8dee9, 0x81a1c1, 0x88c0d0, 0xa3be8c, 0xb48ead, 0x616e88, 0x8fbcbb, 0x81a1c1,
                0xd8dee9,
            ]),
            (CodeTheme::Nord, false) => palette([
                0x2e3440, 0x5e81ac, 0x3b7f99, 0x5c7f42, 0x8a5a83, 0x7b88a1, 0x3b7f7d, 0x5e81ac,
                0x2e3440,
            ]),
            // Monokai; light: Monokai Pro's light filter.
            (CodeTheme::Monokai, true) => palette([
                0xf8f8f2, 0xf92672, 0xa6e22e, 0xe6db74, 0xae81ff, 0x75715e, 0x66d9ef, 0xf92672,
                0xf8f8f2,
            ]),
            (CodeTheme::Monokai, false) => palette([
                0x29242a, 0xe14775, 0x269d69, 0xcc7a0a, 0x7058be, 0xa59fa0, 0x1c8ca8, 0xe14775,
                0x29242a,
            ]),
        })
    }
}

impl Palette {
    /// The `syntax` object of an editor theme, as JSON.
    pub fn syntax_json(&self) -> String {
        let hex = |c: u32| format!("#{c:06x}");
        let entry = |name: &str, color: u32, extra: &str| {
            format!("\"{name}\": {{ \"color\": \"{}\"{extra} }}", hex(color))
        };
        let keyword_extra = if self.bold_keywords {
            ", \"font_weight\": 600"
        } else {
            ""
        };
        [
            entry("keyword", self.keyword, keyword_extra),
            entry("function", self.function, ""),
            entry("string", self.string, ""),
            entry("string.special", self.string, ""),
            entry("number", self.number, ""),
            entry("boolean", self.number, ""),
            entry("constant", self.number, ""),
            entry("comment", self.comment, ", \"font_style\": \"italic\""),
            entry("type", self.kind, ""),
            entry("operator", self.operator, ""),
            entry("punctuation", self.text, ""),
            entry("property", self.property, ""),
            entry("variable", self.text, ""),
        ]
        .join(", ")
    }

    /// A few of its colours, for a swatch.
    pub fn swatch(&self) -> [u32; 4] {
        [self.keyword, self.function, self.string, self.number]
    }
}

/// `json` with each mode's `syntax` object, and editor text colour, replaced by
/// `theme`'s palette for that mode.
pub fn apply(json: &str, theme: CodeTheme) -> String {
    let mut out = json.to_string();
    for (mode, name) in [(Mode::Dark, "Xenon Dark"), (Mode::Light, "Xenon Light")] {
        let Some(palette) = theme.palette(mode) else {
            continue;
        };
        let Some(start) = out.find(&format!("\"name\": \"{name}\"")) else {
            continue;
        };
        let Some(syntax) = out[start..].find("\"syntax\": {").map(|i| start + i) else {
            continue;
        };
        let open = syntax + "\"syntax\": ".len();
        let Some(close) = matching_brace(&out, open) else {
            continue;
        };
        out.replace_range(open..=close, &format!("{{ {} }}", palette.syntax_json()));
        let key = "\"editor.foreground\": \"";
        if let Some(fg) = out[start..syntax].find(key).map(|i| start + i + key.len()) {
            out.replace_range(fg..fg + 7, &format!("#{:06x}", palette.text));
        }
    }
    out
}

/// The `}` closing the `{` at `open`.
fn matching_brace(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0;
    for (i, c) in text[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_theme_replaces_only_the_syntax_colours() {
        let json = include_str!("../../../assets/themes/steel.json");
        assert_eq!(apply(json, CodeTheme::Xenon), json);
        let dracula = apply(json, CodeTheme::Dracula);
        // Still valid, and both modes carry the theme's keyword colour.
        let parsed: serde_json::Value = serde_json::from_str(&dracula).expect("valid JSON");
        let themes = parsed["themes"].as_array().expect("themes");
        let keyword = |ix: usize| {
            themes[ix]["highlight"]["syntax"]["keyword"]["color"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(keyword(0), "#ff79c6");
        assert_eq!(keyword(1), "#a3144d");
        assert_eq!(
            themes[0]["highlight"]["editor.foreground"].as_str(),
            Some("#f8f8f2")
        );
        // Everything else is untouched.
        assert_eq!(
            themes[0]["highlight"]["editor.background"],
            serde_json::from_str::<serde_json::Value>(json).unwrap()["themes"][0]["highlight"]["editor.background"]
        );
    }
}
