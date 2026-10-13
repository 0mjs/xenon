//! The main window: title bar, icon rail, explorer, workspace, inspector, status bar.
//! Panels are empty shells for M0; layout follows PLAN.md, "Design direction".

use gpui::{
    AnyElement, Context, FocusHandle, FontWeight, IntoElement, Render, Rgba, SharedString,
    Subscription, Window, div, prelude::*, px,
};

use crate::theme::{ActiveTheme, MONO_FONT, Mode, Theme, UI_FONT};
use crate::{ToggleExplorer, ToggleInspector};

const TITLEBAR_HEIGHT: f32 = 34.;
const STATUSBAR_HEIGHT: f32 = 24.;
const RAIL_WIDTH: f32 = 44.;
const EXPLORER_WIDTH: f32 = 220.;
const INSPECTOR_WIDTH: f32 = 250.;
const EDITOR_HEIGHT: f32 = 196.;
/// Room for the macOS traffic lights in the transparent title bar.
const TRAFFIC_LIGHT_INSET: f32 = 78.;

pub struct Workspace {
    focus_handle: FocusHandle,
    show_explorer: bool,
    show_inspector: bool,
    _appearance: Subscription,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            cx.set_global(Theme::for_mode(Mode::from_appearance(window.appearance())));
            cx.notify();
        });

        Self {
            focus_handle,
            show_explorer: true,
            show_inspector: true,
            _appearance: appearance,
        }
    }

    fn toggle_explorer(&mut self, _: &ToggleExplorer, _: &mut Window, cx: &mut Context<Self>) {
        self.show_explorer = !self.show_explorer;
        cx.notify();
    }

    fn toggle_inspector(&mut self, _: &ToggleInspector, _: &mut Window, cx: &mut Context<Self>) {
        self.show_inspector = !self.show_inspector;
        cx.notify();
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();

        let body = div()
            .flex()
            .flex_row()
            .flex_1()
            .min_h(px(0.))
            .child(rail(&theme))
            .when(self.show_explorer, |el| el.child(explorer(&theme)))
            .child(center(&theme))
            .when(self.show_inspector, |el| el.child(inspector(&theme)));

        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::toggle_explorer))
            .on_action(cx.listener(Self::toggle_inspector))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.s1)
            .font_family(UI_FONT)
            .text_color(theme.text)
            .text_size(px(12.))
            .child(titlebar(&theme))
            .child(body)
            .child(statusbar(&theme))
    }
}

fn titlebar(theme: &Theme) -> impl IntoElement {
    div()
        .h(px(TITLEBAR_HEIGHT))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .bg(theme.bg)
        .border_b_1()
        .border_color(theme.line)
        .child(div().w(px(TRAFFIC_LIGHT_INSET)).h_full().border_r_1().border_color(theme.line))
        .child(
            div()
                .h_full()
                .px(px(14.))
                .flex()
                .items_center()
                .bg(theme.s1)
                .relative()
                .border_r_1()
                .border_color(theme.line)
                .child("Welcome")
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .h(px(1.))
                        .bg(theme.acc),
                ),
        )
        .child(div().flex_1())
        .child(
            div()
                .mr(px(8.))
                .flex()
                .items_center()
                .gap(px(10.))
                .pl(px(10.))
                .pr(px(5.))
                .py(px(3.))
                .bg(theme.s1)
                .border_1()
                .border_color(theme.line)
                .text_color(theme.t3)
                .text_size(px(11.5))
                .child("Go to anything")
                .child(kbd(theme, "⌘K")),
        )
}

fn rail(theme: &Theme) -> impl IntoElement {
    let icon = |glyph: &'static str, active: bool| {
        div()
            .size(px(30.))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(13.))
            .text_color(if active { theme.text } else { theme.t3 })
            .when(active, |el| el.border_l_2().border_color(theme.acc))
            .child(glyph)
    };

    div()
        .w(px(RAIL_WIDTH))
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .pt(px(8.))
        .gap(px(2.))
        .bg(theme.bg)
        .border_r_1()
        .border_color(theme.line)
        .child(icon("▤", true))
        .child(icon("⌕", false))
        .child(icon("◷", false))
        .child(icon("★", false))
        .child(icon("⚙", false))
}

fn explorer(theme: &Theme) -> impl IntoElement {
    div()
        .w(px(EXPLORER_WIDTH))
        .flex_none()
        .flex()
        .flex_col()
        .border_r_1()
        .border_color(theme.line)
        .child(panel_label(theme, "Explorer"))
        .child(
            div()
                .mx(px(10.))
                .mb(px(6.))
                .px(px(8.))
                .py(px(5.))
                .bg(theme.s2)
                .border_1()
                .border_color(theme.line_2)
                .text_color(theme.t3)
                .text_size(px(11.5))
                .child("⌕ Filter"),
        )
        .child(empty_hint(theme, "No connections yet"))
}

fn center(theme: &Theme) -> impl IntoElement {
    let editor = div()
        .h(px(EDITOR_HEIGHT))
        .flex_none()
        .bg(theme.s2)
        .border_b_1()
        .border_color(theme.line)
        .font_family(MONO_FONT)
        .text_size(px(12.5))
        .py(px(10.))
        .child(
            div()
                .flex()
                .flex_row()
                .bg(theme.s3)
                .child(
                    div()
                        .w(px(42.))
                        .pr(px(14.))
                        .flex()
                        .justify_end()
                        .text_color(theme.t2)
                        .child("1"),
                )
                .child(div().w(px(1.5)).h(px(16.)).bg(theme.acc)),
        );

    let results = div()
        .flex_1()
        .flex()
        .flex_col()
        .child(
            div()
                .h(px(30.))
                .flex_none()
                .flex()
                .items_center()
                .px(px(12.))
                .border_b_1()
                .border_color(theme.line)
                .font_family(MONO_FONT)
                .text_size(px(11.))
                .text_color(theme.t3)
                .child("No results"),
        )
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .child(
                    div()
                        .text_size(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Xenon"),
                )
                .child(
                    div()
                        .text_color(theme.t3)
                        .child("Open a SQLite file or connect to Postgres to begin."),
                ),
        );

    div()
        .flex_1()
        .min_w(px(0.))
        .flex()
        .flex_col()
        .child(editor)
        .child(results)
}

fn inspector(theme: &Theme) -> impl IntoElement {
    div()
        .w(px(INSPECTOR_WIDTH))
        .flex_none()
        .flex()
        .flex_col()
        .border_l_1()
        .border_color(theme.line)
        .child(panel_label(theme, "Inspector"))
        .child(empty_hint(theme, "Select a row or column"))
}

fn statusbar(theme: &Theme) -> impl IntoElement {
    let cell = |text: SharedString, color: Rgba| {
        div()
            .h_full()
            .px(px(12.))
            .flex()
            .items_center()
            .border_r_1()
            .border_color(theme.line)
            .text_color(color)
            .child(text)
    };

    div()
        .h(px(STATUSBAR_HEIGHT))
        .flex_none()
        .flex()
        .flex_row()
        .bg(theme.bg)
        .border_t_1()
        .border_color(theme.line)
        .font_family(MONO_FONT)
        .text_size(px(10.5))
        .child(cell("no connection".into(), theme.t3))
        .child(div().flex_1())
        .child(
            cell(concat!("xenon ", env!("CARGO_PKG_VERSION")).into(), theme.t3)
                .border_l_1(),
        )
}

fn panel_label(theme: &Theme, text: &'static str) -> AnyElement {
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

fn empty_hint(theme: &Theme, text: &'static str) -> AnyElement {
    div()
        .px(px(12.))
        .py(px(6.))
        .text_color(theme.t3)
        .child(text)
        .into_any_element()
}

fn kbd(theme: &Theme, keys: &'static str) -> impl IntoElement {
    div()
        .px(px(5.))
        .bg(theme.s2)
        .border_1()
        .border_color(theme.line)
        .font_family(MONO_FONT)
        .text_size(px(10.))
        .text_color(theme.t2)
        .child(keys)
}
