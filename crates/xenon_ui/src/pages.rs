//! Paging through a table: a page of rows at a time (Settings → Rows when
//! browsing a table), with how many there are in all: counted when that's
//! cheap, otherwise the engine's estimate.

use gpui::{AnyElement, Context, Window, div, prelude::*, px};
use xenon_driver::{QueryOutcome, Value};

use crate::theme::Theme;
use crate::widgets::{self, ButtonKind, group_digits};
use crate::workspace::Workspace;

/// Tables with more rows than this (by the engine's estimate) aren't counted:
/// the estimate is shown instead.
const COUNT_UP_TO: u64 = 100_000;

/// How many counts have started, for tests.
#[cfg(test)]
pub(crate) static COUNTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Total {
    Exact(u64),
    About(u64),
    /// Being counted, so it isn't counted twice.
    Counting,
}

/// `1.2M`, `34K`: a rough count, shown as one.
fn rough(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.0}K", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

/// `select count(*)` for a table, with the filter the tab shows.
pub(crate) fn count_sql(name: &str, filter: &str) -> String {
    let filter = filter.trim().trim_end_matches(';').trim();
    if filter.is_empty() {
        format!("select count(*) from {name}")
    } else {
        format!("select count(*) from {name}\nwhere {filter}")
    }
}

impl Workspace {
    pub(crate) fn turn_page(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(browse) = &mut self.tabs[self.active].browse else {
            return;
        };
        browse.page = if forward {
            browse.page + 1
        } else {
            browse.page.saturating_sub(1)
        };
        self.run_browse(window, cx);
    }

    /// After a table tab's rows arrive: how many there are in all, once per
    /// filter and sort.
    ///
    /// The count runs the tab's filter again, so it gets the same protection as
    /// the browse: only after the browse itself succeeded, never when the count
    /// would write, not while another query uses the connection, and on
    /// production in a read-only transaction under the time limit.
    pub(crate) fn find_total(&mut self, tab_id: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.iter().find(|t| t.id == tab_id) else {
            return;
        };
        if !matches!(tab.query, crate::workspace::Query::Done(_)) || self.busy(tab_id).is_some() {
            return;
        }
        let (Some(browse), Some(session)) =
            (&tab.browse, tab.session.and_then(|id| self.session(id)))
        else {
            return;
        };
        // Counted already, or waiting for the table's size (it may be huge).
        if browse.total.is_some() || browse.info.is_none() {
            return;
        }
        let filter = browse.applied.clone();
        let estimate = browse.info.as_ref().and_then(|info| info.estimated_rows);
        if let Some(estimate) = estimate.filter(|&e| e > COUNT_UP_TO) {
            // Too big to count. Filtered, the estimate says nothing.
            if filter.trim().is_empty() {
                self.set_total(tab_id, Total::About(estimate), cx);
            }
            return;
        }
        let conn = session.conn.clone();
        let schema = (conn.default_schema().as_deref() != Some(browse.schema.as_str()))
            .then(|| conn.ident(&browse.schema));
        let name = match schema {
            Some(schema) => format!("{schema}.{}", conn.ident(&browse.table)),
            None => conn.ident(&browse.table),
        };
        let sql = count_sql(&name, &filter);
        if xenon_driver::sql::writes(&sql) {
            return;
        }
        let production = session.environment == xenon_store::Environment::Production;
        // On production inside the user's own transaction there's no wrapping
        // it (a rollback would end theirs): skip the count.
        if production && session.in_transaction {
            return;
        }
        let wrap = production.then(|| conn.begin_read_only()).flatten();
        let limit = (production && self.prefs.query_limit > 0)
            .then(|| std::time::Duration::from_secs(self.prefs.query_limit));
        let live = std::sync::Arc::new(crate::workspace::Live::default());
        if let Some(limit) = limit {
            crate::workspace::watchdog(live.clone(), conn.cancel_handle(), limit);
        }
        self.set_total(tab_id, Total::Counting, cx);
        #[cfg(test)]
        COUNTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let task = cx.background_spawn(async move {
            let counted = match wrap {
                Some(begin) => conn.execute(begin, 1).and_then(|_| {
                    let counted = conn.execute(&sql, 1);
                    let _ = conn.execute("rollback", 1);
                    counted
                }),
                None => conn.execute(&sql, 1),
            };
            live.done.store(true, std::sync::atomic::Ordering::SeqCst);
            counted
        });
        cx.spawn(async move |this, cx| {
            let Ok(done) = task.await else { return };
            let count = match done.outcome {
                QueryOutcome::Rows(rows) => match rows.rows.first().and_then(|r| r.first()) {
                    Some(Value::Integer(n)) => u64::try_from(*n).ok(),
                    Some(Value::Decimal(n) | Value::Text(n)) => n.parse().ok(),
                    _ => None,
                },
                QueryOutcome::Affected(_) => None,
            };
            if let Some(count) = count {
                this.update(cx, |this, cx| {
                    this.set_total(tab_id, Total::Exact(count), cx)
                })
                .ok();
            }
        })
        .detach();
    }

    fn set_total(&mut self, tab_id: usize, total: Total, cx: &mut Context<Self>) {
        if let Some(browse) = self
            .tabs
            .iter_mut()
            .find(|t| t.id == tab_id)
            .and_then(|t| t.browse.as_mut())
        {
            browse.total = Some(total);
            cx.notify();
        }
    }

    /// `‹ 1–1,000 of 12,345 ›` for a table tab.
    pub(crate) fn render_pager(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let tab = self.active_tab();
        let browse = tab.browse.as_ref()?;
        let grid = tab.grid.as_ref()?;
        let limit = self.prefs.browse_limit;
        let rows = grid.data.rows.len();
        let first = browse.page * limit;
        let more = rows >= limit;
        if browse.page == 0 && !more {
            return None;
        }
        let range = if rows == 0 {
            "no rows".to_string()
        } else {
            format!("{}–{}", group_digits(first + 1), group_digits(first + rows))
        };
        // Narrow, "of" gives way to a slash.
        let compact = self.compact_bar();
        let total = match browse.total {
            Some(Total::Exact(n)) if compact => format!(" / {}", group_digits(n as usize)),
            Some(Total::Exact(n)) => format!(" of {}", group_digits(n as usize)),
            Some(Total::About(n)) if compact => format!(" / ~{}", rough(n)),
            Some(Total::About(n)) => format!(" of about {}", rough(n)),
            Some(Total::Counting) | None => String::new(),
        };
        let arrow = |id: &'static str, label: &'static str, enabled: bool, forward: bool| {
            widgets::button(theme, id, label, ButtonKind::Ghost, enabled)
                .h(px(24.))
                .px(px(8.))
                .when(enabled, |el| {
                    el.on_click(
                        cx.listener(move |this, _, window, cx| this.turn_page(forward, window, cx)),
                    )
                })
        };
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(2.))
                .child(arrow("page-back", "‹", browse.page > 0, false))
                .child(
                    div()
                        .font_family(crate::theme::mono_font())
                        .text_size(crate::theme::text(10.5))
                        .text_color(theme.t2)
                        .child(format!("{range}{total}")),
                )
                .child(arrow("page-next", "›", more, true))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rough_counts_read_short() {
        assert_eq!(rough(950), "950");
        assert_eq!(rough(34_400), "34K");
        assert_eq!(rough(1_234_567), "1.2M");
    }

    #[test]
    fn counts_carry_the_filter() {
        assert_eq!(count_sql("orders", ""), "select count(*) from orders");
        assert_eq!(
            count_sql("orders", " status = 'paid'; "),
            "select count(*) from orders\nwhere status = 'paid'"
        );
    }
}
