//! Arranging a result's columns: hide, pin (kept in view as the grid scrolls
//! sideways) and move, from a header's right-click menu. A tab keeps its
//! arrangement across runs; a table remembers its own, by column name.

use gpui::{AnyElement, Context, Pixels, Point, div, prelude::*, px};

use crate::results::ColumnLayout;
use crate::theme::Theme;
use crate::widgets;
use crate::workspace::Workspace;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColumnAction {
    Hide,
    TogglePin,
    Left,
    Right,
    ShowAll,
}

fn setting_key(connection: &str, schema: &str, table: &str) -> String {
    format!("columns:{connection}:{schema}.{table}")
}

impl Workspace {
    pub(crate) fn open_column_menu(
        &mut self,
        column: usize,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.column_menu = Some((column, at));
        cx.notify();
    }

    pub(crate) fn column_action(&mut self, action: ColumnAction, cx: &mut Context<Self>) {
        let Some((column, _)) = self.column_menu.take() else {
            return;
        };
        let tab = &mut self.tabs[self.active];
        let Some(grid) = &mut tab.grid else {
            return;
        };
        match action {
            ColumnAction::Hide => grid.hide(column),
            ColumnAction::TogglePin => {
                grid.toggle_pin(column);
            }
            ColumnAction::Left => {
                grid.move_column(column, false);
            }
            ColumnAction::Right => {
                grid.move_column(column, true);
            }
            ColumnAction::ShowAll => grid.reset_columns(),
        }
        tab.columns = Some(grid.layout());
        // Columns moved under the selection: start it afresh.
        tab.selection = None;
        self.save_column_layout();
        cx.notify();
    }

    /// Remember the active table tab's arrangement for that table.
    fn save_column_layout(&self) {
        let tab = self.active_tab();
        let (Some(browse), Some(layout), Some(session), Some(store)) = (
            &tab.browse,
            &tab.columns,
            self.active_session(),
            &self.store,
        ) else {
            return;
        };
        let key = setting_key(&session.conn.id(), &browse.schema, &browse.table);
        if let Ok(json) = serde_json::to_string(layout) {
            let _ = store.set_setting(&key, &json);
        }
    }

    /// The arrangement a table tab last had, if it was ever changed.
    pub(crate) fn saved_column_layout(&self, tab_id: usize) -> Option<ColumnLayout> {
        let tab = self.tabs.iter().find(|t| t.id == tab_id)?;
        let browse = tab.browse.as_ref()?;
        let session = self.session(tab.session?)?;
        let json = self
            .store
            .as_ref()?
            .setting(&setting_key(
                &session.conn.id(),
                &browse.schema,
                &browse.table,
            ))
            .ok()??;
        serde_json::from_str(&json).ok()
    }

    pub(crate) fn render_column_menu(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (column, at) = self.column_menu?;
        let grid = self.active_tab().grid.as_ref()?;
        let pinned = column < grid.pinned;
        let shown = grid.columns().len();
        let hidden = grid.hidden();
        let item = |id: &'static str, label: String, enabled: bool, action: ColumnAction| {
            div()
                .id(id)
                .h(px(28.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(px(widgets::RADIUS_CONTROL))
                .when(enabled, |el| {
                    el.cursor_pointer()
                        .text_color(theme.text)
                        .hover(|s| s.bg(theme.s3))
                        .on_click(cx.listener(move |this, _, _, cx| this.column_action(action, cx)))
                })
                .when(!enabled, |el| el.text_color(theme.t3))
                .child(label)
        };
        let same_side_left = column > 0 && (column - 1 < grid.pinned) == pinned;
        let same_side_right = column + 1 < shown && (column + 1 < grid.pinned) == pinned;
        Some(
            gpui::deferred(
                div().absolute().left(at.x).top(at.y).child(
                    widgets::overlay_panel(theme)
                        .id("column-menu")
                        .w(px(200.))
                        .h_auto()
                        .p(px(4.))
                        .text_size(crate::theme::text(12.))
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.column_menu = None;
                            cx.notify();
                        }))
                        .child(item(
                            "column-pin",
                            if pinned { "Unpin" } else { "Pin to the left" }.into(),
                            true,
                            ColumnAction::TogglePin,
                        ))
                        .child(item(
                            "column-left",
                            "Move left".into(),
                            same_side_left,
                            ColumnAction::Left,
                        ))
                        .child(item(
                            "column-right",
                            "Move right".into(),
                            same_side_right,
                            ColumnAction::Right,
                        ))
                        .child(item(
                            "column-hide",
                            "Hide".into(),
                            shown > 1,
                            ColumnAction::Hide,
                        ))
                        .child(div().my(px(4.)).h(px(1.)).bg(theme.line_2))
                        .child(item(
                            "column-show-all",
                            if hidden > 0 {
                                format!("Show all columns ({hidden} hidden)")
                            } else {
                                "Reset columns".into()
                            },
                            hidden > 0
                                || grid.pinned > 0
                                || grid.columns().windows(2).any(|w| w[0] > w[1]),
                            ColumnAction::ShowAll,
                        )),
                ),
            )
            .into_any_element(),
        )
    }
}
