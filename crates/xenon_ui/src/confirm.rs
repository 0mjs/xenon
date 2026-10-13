//! "Are you sure?" before running a risky statement. On production
//! connections the user types the connection's name to continue.

use gpui::{
    Context, Entity, EventEmitter, FocusHandle, FontWeight, IntoElement, Render, SharedString,
    Subscription, Window, div, prelude::*, px, rgba,
};
use gpui_component::input::{Escape, Input, InputEvent, InputState};

use crate::theme::{ActiveTheme, MONO_FONT, Mode};

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

        let preview: String = sql.lines().take(8).collect::<Vec<_>>().join("\n");
        Self {
            focus_handle,
            title: title.into(),
            body: body.into(),
            preview: preview.into(),
            require,
            input,
            _subscription: subscription,
        }
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let unlocked = self.unlocked(cx);

        let panel = div()
            .id("confirm-panel")
            .w(px(480.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(20.))
            .bg(theme.s1)
            .border_1()
            .border_color(theme.line)
            // Clicking anywhere outside the panel closes the dialog.
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(ConfirmEvent::Cancel)))
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.title.clone()),
            )
            .child(
                div()
                    .text_color(theme.t2)
                    .line_height(px(18.))
                    .child(self.body.clone()),
            )
            .child(
                div()
                    .p(px(10.))
                    .bg(theme.s2)
                    .border_l_2()
                    .border_color(theme.err)
                    .font_family(MONO_FONT)
                    .text_size(px(11.))
                    .text_color(theme.t2)
                    .child(self.preview.clone()),
            )
            .when_some(self.require.clone(), |el, text| {
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(
                            div()
                                .text_color(theme.t2)
                                .child(format!("Type \"{text}\" to continue.")),
                        )
                        .child(Input::new(&self.input)),
                )
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        div()
                            .id("confirm-cancel")
                            .px(px(12.))
                            .py(px(6.))
                            .border_1()
                            .border_color(theme.line)
                            .text_color(theme.t2)
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme.text).border_color(theme.t3))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ConfirmEvent::Cancel)))
                            .child("Cancel"),
                    )
                    .child(
                        div()
                            .id("confirm-run")
                            .px(px(12.))
                            .py(px(6.))
                            .border_1()
                            .border_color(theme.err)
                            .font_weight(FontWeight::MEDIUM)
                            .when(unlocked, |el| {
                                el.bg(theme.err).text_color(theme.bg).cursor_pointer()
                            })
                            .when(!unlocked, |el| el.text_color(theme.err).opacity(0.5))
                            .on_click(cx.listener(|this, _, _, cx| this.confirm(cx)))
                            .child("Run anyway"),
                    ),
            );

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
            .bg(match theme.mode {
                Mode::Dark => rgba(0x00000099),
                Mode::Light => rgba(0x17181b40),
            })
            .child(panel)
    }
}
