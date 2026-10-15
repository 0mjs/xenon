//! Preferences, and the Settings window (⌘,) that edits them. Every change is
//! saved and applied as it's made; there's no Save button.

use gpui::{
    Context, EventEmitter, FocusHandle, FontWeight, IntoElement, Render, SharedString, Window, div,
    prelude::*, px,
};
use gpui_component::input::Escape;
use xenon_store::Store;

use crate::theme::{Accent, ActiveTheme, CodeFont, Layout, Mode, Theme, UiFont, ZOOMS};
use crate::widgets::{self, ButtonKind, RADIUS_CONTROL};

/// Light, dark, or whatever macOS is set to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThemeChoice {
    System,
    Light,
    Dark,
}

impl ThemeChoice {
    pub(crate) const ALL: [ThemeChoice; 3] =
        [ThemeChoice::System, ThemeChoice::Light, ThemeChoice::Dark];

    fn parse(value: &str) -> Self {
        match value {
            "light" => ThemeChoice::Light,
            "dark" => ThemeChoice::Dark,
            _ => ThemeChoice::System,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            ThemeChoice::System => "system",
            ThemeChoice::Light => "light",
            ThemeChoice::Dark => "dark",
        }
    }

    fn label(self) -> &'static str {
        match self {
            ThemeChoice::System => "Match System",
            ThemeChoice::Light => "Light",
            ThemeChoice::Dark => "Dark",
        }
    }

    pub(crate) fn mode(self, window: &Window) -> Mode {
        match self {
            ThemeChoice::System => Mode::from_appearance(window.appearance()),
            ThemeChoice::Light => Mode::Light,
            ThemeChoice::Dark => Mode::Dark,
        }
    }
}

/// Rows kept from one result set.
const ROW_LIMITS: [(usize, &str); 4] = [
    (10_000, "10,000"),
    (100_000, "100,000"),
    (1_000_000, "1,000,000"),
    (usize::MAX, "No limit"),
];
/// Rows fetched when a table is opened from the explorer.
const BROWSE_LIMITS: [(usize, &str); 4] = [
    (100, "100"),
    (500, "500"),
    (1_000, "1,000"),
    (5_000, "5,000"),
];
/// How long a query on a production connection may run before Xenon stops it (seconds).
const QUERY_LIMITS: [(u64, &str); 4] = [(0, "Off"), (30, "30 s"), (60, "1 min"), (300, "5 min")];
/// How long a statement on a production connection waits for a lock (seconds).
const LOCK_WAITS: [(u64, &str); 4] = [(0, "Off"), (2, "2 s"), (5, "5 s"), (30, "30 s")];
const EDITOR_FONTS: [(f32, &str); 4] = [
    (12., "Small"),
    (12.5, "Default"),
    (14., "Large"),
    (16., "Larger"),
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Prefs {
    pub theme: ThemeChoice,
    pub accent: Accent,
    pub layout: Layout,
    pub ui_font: UiFont,
    pub code_font: CodeFont,
    pub code_theme: crate::code_themes::CodeTheme,
    /// Text size, as a scale (⌘+, ⌘−): one of [`ZOOMS`].
    pub zoom: f32,
    /// The desktop shows, blurred, through the window's frame.
    pub frosted: bool,
    pub row_limit: usize,
    pub browse_limit: usize,
    pub editor_font: f32,
    /// Ask before statements that change every row or can't be undone, on
    /// connections that aren't production. (Production always asks.)
    pub confirm_risky: bool,
    /// Reopen last session's tabs (on saved connections) at launch.
    pub restore_tabs: bool,
    /// On production: stop a query after this many seconds (0: never).
    pub query_limit: u64,
    /// On production: give up waiting for a lock after this many seconds (0: wait).
    pub lock_wait: u64,
    /// Keep history (and reopened tabs) for production connections.
    pub production_history: bool,
    /// Ask GitHub for a newer release at launch (at most daily).
    pub check_updates: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::System,
            accent: Accent::default(),
            layout: Layout::default(),
            ui_font: UiFont::default(),
            code_font: CodeFont::default(),
            code_theme: Default::default(),
            zoom: 1.,
            frosted: true,
            row_limit: 100_000,
            browse_limit: 1_000,
            editor_font: 12.5,
            confirm_risky: true,
            restore_tabs: true,
            query_limit: 60,
            lock_wait: 5,
            production_history: true,
            check_updates: true,
        }
    }
}

impl Prefs {
    pub(crate) fn load(store: Option<&Store>) -> Self {
        let mut prefs = Self::default();
        let Some(store) = store else { return prefs };
        let get = |key: &str| store.setting(key).ok().flatten();
        if let Some(value) = get("theme") {
            prefs.theme = ThemeChoice::parse(&value);
        }
        if let Some(value) = get(Accent::SETTING) {
            prefs.accent = Accent::parse(&value);
        }
        if let Some(value) = get("layout") {
            prefs.layout = Layout::parse(&value);
        }
        if let Some(value) = get("ui_font") {
            prefs.ui_font = UiFont::parse(&value);
        }
        if let Some(value) = get("code_theme") {
            prefs.code_theme = crate::code_themes::CodeTheme::parse(&value);
        }
        if let Some(value) = get("code_font") {
            prefs.code_font = CodeFont::parse(&value);
        }
        if let Some(value) = get("zoom").and_then(|v| v.parse::<f32>().ok()) {
            prefs.zoom = value.clamp(ZOOMS[0], ZOOMS[ZOOMS.len() - 1]);
        }
        if let Some(value) = get("frosted") {
            prefs.frosted = value != "false";
        }
        if let Some(value) = get("row_limit").and_then(|v| v.parse().ok()) {
            prefs.row_limit = value;
        }
        if let Some(value) = get("browse_limit").and_then(|v| v.parse().ok()) {
            prefs.browse_limit = value;
        }
        if let Some(value) = get("editor_font").and_then(|v| v.parse().ok()) {
            prefs.editor_font = value;
        }
        if let Some(value) = get("confirm_risky") {
            prefs.confirm_risky = value != "false";
        }
        if let Some(value) = get("restore_tabs") {
            prefs.restore_tabs = value != "false";
        }
        if let Some(value) = get("query_limit").and_then(|v| v.parse().ok()) {
            prefs.query_limit = value;
        }
        if let Some(value) = get("lock_wait").and_then(|v| v.parse().ok()) {
            prefs.lock_wait = value;
        }
        if let Some(value) = get("production_history") {
            prefs.production_history = value != "false";
        }
        if let Some(value) = get("check_updates") {
            prefs.check_updates = value != "false";
        }
        prefs
    }

    pub(crate) fn save(&self, store: Option<&Store>) {
        let Some(store) = store else { return };
        let values = [
            ("theme", self.theme.as_str().to_string()),
            (Accent::SETTING, self.accent.as_str().to_string()),
            ("layout", self.layout.as_str().to_string()),
            ("ui_font", self.ui_font.as_str().to_string()),
            ("code_font", self.code_font.as_str().to_string()),
            ("code_theme", self.code_theme.as_str().to_string()),
            ("zoom", self.zoom.to_string()),
            ("frosted", self.frosted.to_string()),
            ("row_limit", self.row_limit.to_string()),
            ("browse_limit", self.browse_limit.to_string()),
            ("editor_font", self.editor_font.to_string()),
            ("confirm_risky", self.confirm_risky.to_string()),
            ("restore_tabs", self.restore_tabs.to_string()),
            ("query_limit", self.query_limit.to_string()),
            ("lock_wait", self.lock_wait.to_string()),
            ("production_history", self.production_history.to_string()),
            ("check_updates", self.check_updates.to_string()),
        ];
        for (key, value) in values {
            if let Err(err) = store.set_setting(key, &value) {
                eprintln!("xenon: could not save the {key} setting: {err:#}");
            }
        }
    }
}

/// Sets a preference from the index of the option picked.
type Pick = Box<dyn Fn(&mut Prefs, usize)>;

pub(crate) enum SettingsEvent {
    Changed(Prefs),
    ClearHistory,
    Dismiss,
}

pub(crate) struct SettingsDialog {
    focus_handle: FocusHandle,
    prefs: Prefs,
    /// "Clear history" asks once more before it clears.
    clear_armed: bool,
    cleared: bool,
}

impl EventEmitter<SettingsEvent> for SettingsDialog {}

impl SettingsDialog {
    pub(crate) fn new(prefs: Prefs, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        Self {
            focus_handle,
            prefs,
            clear_armed: false,
            cleared: false,
        }
    }

    fn change(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Prefs)) {
        edit(&mut self.prefs);
        self.clear_armed = false;
        cx.emit(SettingsEvent::Changed(self.prefs));
        cx.notify();
    }

    /// Code themes, each with a swatch of its colours in the current mode.
    fn render_code_themes(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::code_themes::CodeTheme;
        div()
            .flex()
            .flex_wrap()
            .gap(px(8.))
            .children(CodeTheme::ALL.into_iter().enumerate().map(|(ix, code)| {
                let on = code == self.prefs.code_theme;
                let colors: Vec<gpui::Rgba> = match code.palette(theme.mode) {
                    Some(p) => p.swatch().into_iter().map(gpui::rgb).collect(),
                    None => vec![theme.text, theme.acc, theme.ok, theme.warn],
                };
                div()
                    .id(("code-theme", ix))
                    .w(px(136.))
                    .h(px(36.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(RADIUS_CONTROL + 1.))
                    .border_1()
                    .border_color(if on { theme.acc } else { theme.line })
                    .bg(if on { theme.acc_wash } else { theme.field })
                    .cursor_pointer()
                    .hover(|s| s.border_color(theme.t3))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.change(cx, |p| p.code_theme = code)),
                    )
                    .child(
                        div().flex().flex_none().gap(px(2.)).children(
                            colors
                                .into_iter()
                                .map(|c| div().size(px(7.)).rounded_full().bg(c)),
                        ),
                    )
                    .child(
                        div()
                            .text_size(crate::theme::text(12.))
                            .truncate()
                            .text_color(if on { theme.text } else { theme.t2 })
                            .when(on, |el| el.font_weight(FontWeight::MEDIUM))
                            .child(code.label()),
                    )
            }))
    }

    fn render_accents(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_wrap()
            .gap(px(8.))
            .children(Accent::ALL.into_iter().map(|accent| {
                let on = accent == self.prefs.accent;
                div()
                    .id(("accent", accent as usize))
                    // Three to a row: two even rows of three.
                    .w(px(184.))
                    .h(px(36.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(RADIUS_CONTROL + 1.))
                    .border_1()
                    .border_color(if on { theme.acc } else { theme.line })
                    .bg(if on { theme.acc_wash } else { theme.field })
                    .cursor_pointer()
                    .hover(|s| s.border_color(theme.t3))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.change(cx, |p| p.accent = accent)),
                    )
                    .child(
                        div()
                            .size(px(14.))
                            .flex_none()
                            .rounded_full()
                            .bg(accent.swatch(theme.mode))
                            .border_1()
                            .border_color(theme.line),
                    )
                    .child(
                        div()
                            .text_size(crate::theme::text(12.))
                            .text_color(if on { theme.text } else { theme.t2 })
                            .when(on, |el| el.font_weight(FontWeight::MEDIUM))
                            .child(accent.label()),
                    )
            }))
    }
}

/// A row in a section: what the setting is (with a hint), and its control.
fn row(
    theme: &Theme,
    label: &'static str,
    hint: Option<&'static str>,
    control: impl IntoElement,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(16.))
        .py(px(10.))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().text_color(theme.text).child(label))
                .when_some(hint, |el, hint| {
                    el.child(
                        div()
                            .text_size(crate::theme::text(11.5))
                            .line_height(px(16.))
                            .text_color(theme.t3)
                            .child(hint),
                    )
                }),
        )
        .child(div().flex_none().child(control))
}

fn section(theme: &Theme, title: &'static str, rows: Vec<gpui::AnyElement>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .pb(px(4.))
                .font_family(crate::theme::mono_font())
                .text_size(crate::theme::text(10.))
                .text_color(theme.t3)
                .child(title.to_uppercase()),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .border_t_1()
                .border_color(theme.line_2)
                .children(rows),
        )
}

impl Render for SettingsDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let prefs = self.prefs;

        let segmented_of = |cx: &mut Context<Self>,
                            id: &'static str,
                            options: Vec<(&'static str, bool)>,
                            pick: Pick| {
            widgets::segmented(
                &theme,
                id,
                options,
                cx.listener(move |this, ix: &usize, _, cx| {
                    let ix = *ix;
                    this.change(cx, |p| pick(p, ix))
                }),
            )
        };

        let appearance = section(
            &theme,
            "Appearance",
            vec![
                row(
                    &theme,
                    "Theme",
                    None,
                    segmented_of(
                        cx,
                        "theme",
                        ThemeChoice::ALL
                            .into_iter()
                            .map(|t| (t.label(), t == prefs.theme))
                            .collect(),
                        Box::new(|p, ix| p.theme = ThemeChoice::ALL[ix]),
                    )
                    .w(px(270.)),
                )
                .into_any_element(),
                row(
                    &theme,
                    "Layout",
                    Some(
                        "Classic: panels edge to edge. Floating: the work area in a rounded card.",
                    ),
                    segmented_of(
                        cx,
                        "layout",
                        Layout::ALL
                            .into_iter()
                            .map(|l| (l.label(), l == prefs.layout))
                            .collect(),
                        Box::new(|p, ix| p.layout = Layout::ALL[ix]),
                    )
                    .w(px(270.)),
                )
                .into_any_element(),
                row(
                    &theme,
                    "Interface font",
                    None,
                    segmented_of(
                        cx,
                        "ui-font",
                        UiFont::ALL
                            .into_iter()
                            .map(|f| (f.label(), f == prefs.ui_font))
                            .collect(),
                        Box::new(|p, ix| p.ui_font = UiFont::ALL[ix]),
                    )
                    .w(px(360.)),
                )
                .into_any_element(),
                row(
                    &theme,
                    "Code font",
                    Some("The editor, results and anything set in a fixed-width face."),
                    segmented_of(
                        cx,
                        "code-font",
                        CodeFont::ALL
                            .into_iter()
                            .map(|f| (f.label(), f == prefs.code_font))
                            .collect(),
                        Box::new(|p, ix| p.code_font = CodeFont::ALL[ix]),
                    )
                    .w(px(360.)),
                )
                .into_any_element(),
                row(
                    &theme,
                    "Text size",
                    Some("Everything, the editor too: ⌘+ and ⌘− step it, ⌘0 resets."),
                    segmented_of(
                        cx,
                        "zoom",
                        ZOOMS
                            .iter()
                            .zip(["85%", "92%", "100%", "108%", "116%", "125%", "135%"])
                            .map(|(z, label)| (label, (*z - prefs.zoom).abs() < 0.001))
                            .collect(),
                        Box::new(|p, ix| p.zoom = ZOOMS[ix]),
                    )
                    .w(px(360.)),
                )
                .into_any_element(),
                row(
                    &theme,
                    "Window",
                    Some("Frosted: the desktop shows, blurred, through the window's frame."),
                    segmented_of(
                        cx,
                        "window-material",
                        vec![("Solid", !prefs.frosted), ("Frosted", prefs.frosted)],
                        Box::new(|p, ix| p.frosted = ix == 1),
                    )
                    .w(px(270.)),
                )
                .into_any_element(),
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .py(px(10.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(div().text_color(theme.text).child("Accent"))
                            .child(
                                div()
                                    .text_size(crate::theme::text(11.5))
                                    .text_color(theme.t3)
                                    .child("Focus, selection and the main button."),
                            ),
                    )
                    .child(self.render_accents(&theme, cx))
                    .into_any_element(),
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .py(px(10.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(div().text_color(theme.text).child("Code theme"))
                            .child(
                                div()
                                    .text_size(crate::theme::text(11.5))
                                    .text_color(theme.t3)
                                    .child("Syntax colours in the editor, light and dark."),
                            ),
                    )
                    .child(self.render_code_themes(&theme, cx))
                    .into_any_element(),
            ],
        );

        let queries = section(
            &theme,
            "Queries",
            vec![
                row(
                    &theme,
                    "Rows kept from a result",
                    Some("More rows use more memory; the rest are skipped and flagged."),
                    segmented_of(
                        cx,
                        "row-limit",
                        ROW_LIMITS
                            .iter()
                            .map(|(n, l)| (*l, *n == prefs.row_limit))
                            .collect(),
                        Box::new(|p, ix| p.row_limit = ROW_LIMITS[ix].0),
                    )
                    .w(px(270.)),
                )
                .into_any_element(),
                row(
                    &theme,
                    "Rows when browsing a table",
                    None,
                    segmented_of(
                        cx,
                        "browse-limit",
                        BROWSE_LIMITS
                            .iter()
                            .map(|(n, l)| (*l, *n == prefs.browse_limit))
                            .collect(),
                        Box::new(|p, ix| p.browse_limit = BROWSE_LIMITS[ix].0),
                    )
                    .w(px(270.)),
                )
                .into_any_element(),
                row(
                    &theme,
                    "Editor text size",
                    None,
                    segmented_of(
                        cx,
                        "editor-font",
                        EDITOR_FONTS
                            .iter()
                            .map(|(size, l)| (*l, (*size - prefs.editor_font).abs() < 0.01))
                            .collect(),
                        Box::new(|p, ix| p.editor_font = EDITOR_FONTS[ix].0),
                    )
                    .w(px(270.)),
                )
                .into_any_element(),
            ],
        );

        let safety = section(
            &theme,
            "Safety",
            vec![
                row(
                    &theme,
                    "Ask before risky statements",
                    Some(
                        "DROP, TRUNCATE, and UPDATE or DELETE without WHERE, on connections that aren't \
                         production. Production always asks, and wants its name typed.",
                    ),
                    div()
                        .id("confirm-risky")
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.change(cx, |p| p.confirm_risky = !p.confirm_risky)
                        }))
                        .child(widgets::checkbox(&theme, prefs.confirm_risky)),
                )
                .into_any_element(),
                row(
                    &theme,
                    "Production query time limit",
                    Some("Xenon stops a query on a production connection that runs longer than this."),
                    segmented_of(
                        cx,
                        "query-limit",
                        QUERY_LIMITS
                            .iter()
                            .map(|(n, l)| (*l, *n == prefs.query_limit))
                            .collect(),
                        Box::new(|p, ix| p.query_limit = QUERY_LIMITS[ix].0),
                    )
                    .w(px(270.)),
                )
                .into_any_element(),
                row(
                    &theme,
                    "Production lock wait",
                    Some(
                        "A statement waiting this long for another session's lock fails instead of \
                         queuing everyone behind it. Applies from the next connect.",
                    ),
                    segmented_of(
                        cx,
                        "lock-wait",
                        LOCK_WAITS
                            .iter()
                            .map(|(n, l)| (*l, *n == prefs.lock_wait))
                            .collect(),
                        Box::new(|p, ix| p.lock_wait = LOCK_WAITS[ix].0),
                    )
                    .w(px(270.)),
                )
                .into_any_element(),
            ],
        );

        let clear_label: SharedString = match (self.cleared, self.clear_armed) {
            (true, _) => "History cleared".into(),
            (false, true) => "Click again to clear".into(),
            (false, false) => "Clear history…".into(),
        };
        let data = section(
            &theme,
            "Data",
            vec![
                row(
                    &theme,
                    "Reopen tabs at launch",
                    Some(
                        "Reconnects the saved connections you had open and brings back their tabs.",
                    ),
                    div()
                        .id("restore-tabs")
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.change(cx, |p| p.restore_tabs = !p.restore_tabs)
                        }))
                        .child(widgets::checkbox(&theme, prefs.restore_tabs)),
                )
                .into_any_element(),
                row(
                    &theme,
                    "Keep history for production",
                    Some(
                        "Off: queries on production connections aren't recorded, and their tabs \
                         aren't reopened, so their SQL never reaches this Mac's disk.",
                    ),
                    div()
                        .id("production-history")
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.change(cx, |p| p.production_history = !p.production_history)
                        }))
                        .child(widgets::checkbox(&theme, prefs.production_history)),
                )
                .into_any_element(),
                row(
                    &theme,
                    "Check for updates at launch",
                    Some(
                        "Once a day, asks GitHub whether a newer Xenon is out. Nothing about you \
                         or your databases is sent. Xenon → Check for Updates… asks any time.",
                    ),
                    div()
                        .id("check-updates")
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.change(cx, |p| p.check_updates = !p.check_updates)
                        }))
                        .child(widgets::checkbox(&theme, prefs.check_updates)),
                )
                .into_any_element(),
                row(
                    &theme,
                    "Query history",
                    Some("Every query you've run, on every connection. Saved connections stay."),
                    widgets::button(
                        &theme,
                        "clear-history",
                        clear_label,
                        if self.clear_armed {
                            ButtonKind::Danger
                        } else {
                            ButtonKind::Secondary
                        },
                        !self.cleared,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.cleared {
                            return;
                        }
                        if this.clear_armed {
                            this.clear_armed = false;
                            this.cleared = true;
                            cx.emit(SettingsEvent::ClearHistory);
                        } else {
                            this.clear_armed = true;
                        }
                        cx.notify();
                    })),
                )
                .into_any_element(),
            ],
        );

        let room = f32::from(window.viewport_size().height) - 200.;
        let body = div()
            .id("settings-body")
            .overflow_y_scroll()
            .max_h(px(room.max(260.)))
            .flex()
            .flex_col()
            .gap(px(22.))
            .px(px(24.))
            .pt(px(4.))
            .pb(px(20.))
            .child(appearance)
            .child(queries)
            .child(safety)
            .child(data);

        let panel = widgets::overlay_panel(&theme)
            .id("settings-panel")
            .w(px(620.))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::Dismiss)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .px(px(24.))
                    .pt(px(22.))
                    .pb(px(16.))
                    .child(
                        div()
                            .text_size(crate::theme::text(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Settings"),
                    )
                    .child(
                        div()
                            .text_size(crate::theme::text(11.5))
                            .text_color(theme.t3)
                            .child("Changes apply and are saved as you make them."),
                    ),
            )
            .child(body)
            .child(
                div()
                    .flex()
                    .items_center()
                    .px(px(24.))
                    .py(px(14.))
                    .border_t_1()
                    .border_color(theme.line_2)
                    .child(
                        div()
                            .font_family(crate::theme::mono_font())
                            .text_size(crate::theme::text(11.))
                            .text_color(theme.t3)
                            .child(concat!("Xenon ", env!("CARGO_PKG_VERSION"))),
                    )
                    .child(div().flex_1())
                    .child(
                        widgets::button(&theme, "settings-done", "Done", ButtonKind::Primary, true)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::Dismiss))),
                    ),
            );

        div()
            .id("settings-scrim")
            .key_context("SettingsDialog")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &Escape, _, cx| cx.emit(SettingsEvent::Dismiss)))
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.scrim)
            .child(panel)
    }
}
