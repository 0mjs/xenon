//! The history panel: every query run on the current connection, newest first.

use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{AnyElement, Context, IntoElement, SharedString, Window, div, prelude::*, px};
use xenon_store::HistoryEntry;

use crate::theme::Theme;
use crate::widgets::{empty_hint, group_digits, search_field};
use crate::workspace::{RunScope, Workspace};

/// Entries shown in the panel; the store keeps more.
const HISTORY_SHOWN: usize = 300;

impl Workspace {
    /// Reload the panel's entries from the store, applying the filter.
    pub(crate) fn reload_history(&mut self, cx: &mut Context<Self>) {
        let (Some(store), Some(session)) = (self.store.clone(), self.active_session()) else {
            self.history.clear();
            cx.notify();
            return;
        };
        let connection = session.conn.id();
        let filter = self.history_filter.read(cx).value().to_string();
        let task =
            cx.background_spawn(async move { store.history(&connection, &filter, HISTORY_SHOWN) });
        cx.spawn(async move |this, cx| {
            let entries = task.await;
            this.update(cx, |this, cx| {
                this.history = entries.unwrap_or_default();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn use_history_entry(
        &mut self,
        sql: String,
        run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = self.active_tab().editor.clone();
        editor.update(cx, |editor, cx| {
            editor.set_value(sql, window, cx);
            editor.focus(window, cx);
        });
        if run {
            self.run(RunScope::All, window, cx);
        }
    }

    pub(crate) fn render_history(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut panel = div().flex_1().flex().flex_col().min_h(px(0.)).child(
            div().px(px(10.)).pb(px(8.)).child(search_field(
                theme,
                &self.history_filter,
                self.field_focus.history,
            )),
        );

        if self.active_session().is_none() {
            return panel
                .child(empty_hint(
                    theme,
                    "Connect to a database to see its history",
                ))
                .into_any_element();
        }
        if self.history.is_empty() {
            return panel
                .child(empty_hint(theme, "Nothing yet"))
                .into_any_element();
        }

        let now = now_ms();
        let mut entries = Vec::with_capacity(self.history.len());
        for entry in &self.history {
            entries.push(self.render_entry(entry, now, theme, cx));
        }
        panel = panel.child(
            div()
                .id("history-list")
                .flex_1()
                .overflow_y_scroll()
                .children(entries),
        );
        panel.into_any_element()
    }

    fn render_entry(
        &self,
        entry: &HistoryEntry,
        now: i64,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let preview: String = entry
            .sql
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(160)
            .collect();
        let outcome: SharedString = match (&entry.error, entry.rows) {
            (Some(_), _) => "error".into(),
            (None, Some(rows)) => format!("{} rows", group_digits(rows as usize)).into(),
            (None, None) => "".into(),
        };
        let sql = entry.sql.clone();

        div()
            .id(SharedString::from(format!("history-{}", entry.id)))
            .px(px(12.))
            .py(px(7.))
            .flex()
            .flex_col()
            .gap(px(3.))
            // Classic: rows split by hairlines. Floating: rounded, set in.
            .map(|el| {
                if self.classic() {
                    el.border_b_1().border_color(theme.line_2)
                } else {
                    el.mx(px(6.)).px(px(8.)).rounded(px(6.))
                }
            })
            .cursor_pointer()
            .hover(|s| s.bg(theme.s3))
            .on_click(
                cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    // Click loads the query; double-click runs it.
                    this.use_history_entry(sql.clone(), event.click_count() >= 2, window, cx);
                }),
            )
            .child(
                div()
                    .font_family(crate::theme::mono_font())
                    .text_size(crate::theme::text(11.))
                    .text_color(theme.text)
                    .truncate()
                    .child(preview),
            )
            .child(
                div()
                    .flex()
                    .gap(px(10.))
                    .font_family(crate::theme::mono_font())
                    .text_size(crate::theme::text(10.))
                    .text_color(theme.t3)
                    .child(ago(now - entry.ran_at))
                    .child(format!("{} ms", entry.duration_ms))
                    .child(
                        div()
                            .when(entry.error.is_some(), |el| el.text_color(theme.err))
                            .child(outcome),
                    ),
            )
            .into_any_element()
    }
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Compact relative time: "now", "5m", "3h", "2d".
pub(crate) fn ago(elapsed_ms: i64) -> String {
    let seconds = elapsed_ms.max(0) / 1000;
    match seconds {
        0..60 => "now".into(),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::ago;

    #[test]
    fn relative_times() {
        assert_eq!(ago(-5), "now");
        assert_eq!(ago(59_000), "now");
        assert_eq!(ago(61_000), "1m");
        assert_eq!(ago(2 * 3_600_000), "2h");
        assert_eq!(ago(3 * 86_400_000), "3d");
    }
}
