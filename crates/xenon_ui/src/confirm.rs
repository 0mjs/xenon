//! "Are you sure?" before running a risky statement. On production
//! connections the user types the connection's name to continue.

use gpui::{
    Context, Entity, EventEmitter, FocusHandle, FontWeight, IntoElement, Render, SharedString,
    Subscription, Window, div, prelude::*, px,
};
use gpui_component::input::{Escape, InputEvent, InputState};

use crate::theme::{ActiveTheme, MONO_FONT};
use crate::widgets::{self, ButtonKind, RADIUS_CONTROL};

pub enum ConfirmEvent {
    Confirm,
    Cancel,
}

pub struct ConfirmDialog {
    focus_handle: FocusHandle,
    title: SharedString,
    body: SharedString,
    preview: SharedString,
    /// Text the user must type to enable the button, if any.
    require: Option<String>,
    /// The confirming button's label, and whether it's red.
    action: (SharedString, bool),
    input: Entity<InputState>,
    _subscription: Subscription,
}

impl EventEmitter<ConfirmEvent> for ConfirmDialog {}

impl ConfirmDialog {
    pub fn new(
        title: impl Into<SharedString>,
        body: impl Into<SharedString>,
        sql: &str,
        require: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx
            .new(|cx| InputState::new(window, cx).placeholder(require.clone().unwrap_or_default()));
        let subscription =
            cx.subscribe_in(
                &input,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => cx.notify(),
                    InputEvent::PressEnter { .. } => this.confirm(cx),
                    _ => {}
                },
            );
        let focus_handle = cx.focus_handle();
        if require.is_some() {
            input.update(cx, |input, cx| input.focus(window, cx));
        } else {
            focus_handle.focus(window, cx);
        }

        let lines: Vec<&str> = sql.lines().collect();
        let mut preview = lines
            .iter()
            .take(12)
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
        if lines.len() > 12 {
            preview.push_str(&format!("\n… and {} more lines", lines.len() - 12));
        }
        Self {
            focus_handle,
            title: title.into(),
            body: body.into(),
            preview: preview.into(),
            require,
            action: ("Run anyway".into(), true),
            input,
            _subscription: subscription,
        }
    }

    /// Label the confirming button (instead of "Run anyway"); `danger` makes it red.
    pub fn action(mut self, label: impl Into<SharedString>, danger: bool) -> Self {
        self.action = (label.into(), danger);
        self
    }

    fn unlocked(&self, cx: &Context<Self>) -> bool {
        self.require
            .as_ref()
            .is_none_or(|text| self.input.read(cx).value().trim() == text)
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if self.unlocked(cx) {
            cx.emit(ConfirmEvent::Confirm);
        }
    }
}

impl Render for ConfirmDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let unlocked = self.unlocked(cx);
        let input_focused = widgets::is_focused(&self.input, window, cx);

        let header = div()
            .flex()
            .gap(px(14.))
            .child(
                div()
                    .size(px(34.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(gpui::Rgba {
                        a: 0.14,
                        ..theme.err
                    })
                    .child(
                        gpui::svg()
                            .path(crate::assets::icon("triangle-alert"))
                            .size(px(17.))
                            .text_color(theme.err),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(5.))
                    .pt(px(1.))
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.title.clone()),
                    )
                    .child(
                        div()
                            .text_color(theme.t2)
                            .line_height(px(18.))
                            .child(self.body.clone()),
                    ),
            );

        let preview = div()
            .px(px(12.))
            .py(px(10.))
            .rounded(px(RADIUS_CONTROL))
            .bg(theme.field)
            .border_1()
            .border_color(theme.line)
            .font_family(MONO_FONT)
            .text_size(px(11.5))
            .line_height(px(17.))
            .text_color(theme.text)
            .child(self.preview.clone());

        let body = div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .p(px(20.))
            .child(header)
            .child(preview)
            .when_some(self.require.clone(), |el, text| {
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(7.))
                        .child(
                            div()
                                .flex()
                                .gap(px(4.))
                                .text_size(px(11.5))
                                .text_color(theme.t2)
                                .child("Type")
                                .child(
                                    div()
                                        .font_family(MONO_FONT)
                                        .text_color(theme.text)
                                        .child(text),
                                )
                                .child("to confirm"),
                        )
                        .child(widgets::text_field(&theme, &self.input, input_focused)),
                )
            });

        let footer = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(20.))
            .py(px(14.))
            .border_t_1()
            .border_color(theme.line_2)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(11.))
                    .text_color(theme.t3)
                    .child(widgets::kbd(&theme, "esc"))
                    .child("to cancel"),
            )
            .child(div().flex_1())
            .child(
                widgets::button(&theme, "confirm-cancel", "Cancel", ButtonKind::Ghost, true)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ConfirmEvent::Cancel))),
            )
            .child(
                widgets::button(
                    &theme,
                    "confirm-run",
                    self.action.0.clone(),
                    if self.action.1 {
                        ButtonKind::Danger
                    } else {
                        ButtonKind::Primary
                    },
                    unlocked,
                )
                .on_click(cx.listener(|this, _, _, cx| this.confirm(cx))),
            );

        let panel = widgets::overlay_panel(&theme)
            .id("confirm-panel")
            .w(px(460.))
            // Clicking anywhere outside the panel closes the dialog.
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(ConfirmEvent::Cancel)))
            .child(body)
            .child(footer);

        div()
            .id("confirm-scrim")
            .key_context("ConfirmDialog")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &Escape, _, cx| cx.emit(ConfirmEvent::Cancel)))
            .on_action(
                cx.listener(|_, _: &crate::FocusEditor, _, cx| cx.emit(ConfirmEvent::Cancel)),
            )
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
