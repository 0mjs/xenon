//! Saved queries (the panel, ⌘S and naming one) and reopening last session's tabs.

use std::collections::HashMap;

use gpui::{
    AnyElement, Context, Entity, FontWeight, IntoElement, SharedString, Subscription, Window, div,
    prelude::*, px, svg,
};
use gpui_component::input::{InputEvent, InputState};
use xenon_store::{OpenTab, SavedQuery};

use crate::assets;
use crate::confirm::ConfirmDialog;
use crate::history::{ago, now_ms};
use crate::theme::{MONO_FONT, Theme};
use crate::widgets::{self, ButtonKind, empty_hint, search_field};
use crate::workspace::{Pending, RunScope, Workspace};

/// The name being typed for a query about to be saved.
pub(crate) struct Naming {
    pub input: Entity<InputState>,
    pub tab_id: usize,
    /// Save as a new query even when the tab already has one.
    pub copy: bool,
    pub _subscription: Subscription,
}

impl Workspace {
    /// Reload the Saved panel from the store, applying its filter.
    pub(crate) fn reload_saved_queries(&mut self, cx: &mut Context<Self>) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let filter = self.saved_filter.read(cx).value().to_string();
        let task = cx.background_spawn(async move { store.saved_queries(&filter) });
        cx.spawn(async move |this, cx| {
            let queries = task.await;
            this.update(cx, |this, cx| {
                this.saved_queries = queries.unwrap_or_default();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Keep the active tab's SQL: update its saved query, or ask for a name.
    /// `copy` always asks, and saves a new query.
    pub(crate) fn save_query(&mut self, copy: bool, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.active_tab();
        if tab.session.is_none() {
            return;
        }
        let sql = tab.editor.read(cx).value().to_string();
        if sql.trim().is_empty() {
            self.flash("Nothing to save yet.", true, cx);
            return;
        }
        match tab.saved_query.filter(|_| !copy) {
            Some(id) => {
                let (tab_id, name) = (tab.id, tab.title.to_string());
                self.store_query(tab_id, id, name, sql, cx);
            }
            None => self.ask_query_name(copy, window, cx),
        }
    }

    fn ask_query_name(&mut self, copy: bool, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.active_tab();
        let suggested = match &tab.browse {
            Some(browse) => browse.table.clone(),
            None if tab.title.starts_with("Query ") => String::new(),
            None => tab.title.to_string(),
        };
        let tab_id = tab.id;
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("e.g. Reports/Paid orders this week (a slash files it in a folder)")
        });
        input.update(cx, |input, cx| {
            input.set_value(suggested, window, cx);
            input.focus(window, cx);
        });
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.finish_naming(window, cx);
                }
            });
        self.naming = Some(Naming {
            input,
            tab_id,
            copy,
            _subscription: subscription,
        });
        cx.notify();
    }

    pub(crate) fn finish_naming(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(naming) = &self.naming else { return };
        let name = naming.input.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        let naming = self.naming.take().expect("checked above");
        let Some(tab) = self.tabs.iter().find(|t| t.id == naming.tab_id) else {
            return;
        };
        let sql = tab.editor.read(cx).value().to_string();
        let id = if naming.copy {
            0
        } else {
            tab.saved_query.unwrap_or(0)
        };
        self.store_query(naming.tab_id, id, name, sql, cx);
        self.focus_editor(window, cx);
    }

    pub(crate) fn cancel_naming(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.naming = None;
        self.focus_editor(window, cx);
        cx.notify();
    }

    fn store_query(
        &mut self,
        tab_id: usize,
        id: i64,
        name: String,
        sql: String,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = self.store.clone() else {
            self.flash(
                "Saving queries needs Xenon's data file, which couldn't be opened.",
                true,
                cx,
            );
            return;
        };
        let connection = self.active_session().and_then(|s| s.saved_id);
        let query = SavedQuery {
            id,
            name: name.clone(),
            sql,
            connection,
            updated_at: 0,
        };
        let saved = store.save_query(&query);
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) else {
            return;
        };
        tab.flash = Some(match saved {
            Ok(id) => {
                tab.saved_query = Some(id);
                // A table tab keeps its table name.
                if tab.browse.is_none() {
                    tab.title = name.clone().into();
                }
                (format!("Saved “{name}”").into(), false)
            }
            Err(err) => (format!("{err:#}").into(), true),
        });
        self.reload_saved_queries(cx);
        cx.notify();
    }

    /// Load a saved query into a tab (its own, if one has it open); `run` runs it too.
    fn open_saved_query(
        &mut self,
        query: SavedQuery,
        run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.active_session().map(|s| s.id) else {
            self.notice = Some("Connect to a database to open a saved query.".into());
            cx.notify();
            return;
        };
        if let Some(index) = self
            .tabs
            .iter()
            .position(|t| t.saved_query == Some(query.id) && t.session == Some(session))
        {
            self.activate_tab(index, window, cx);
        } else {
            let tab = self.active_tab();
            let reuse = tab.browse.is_none()
                && tab.saved_query.is_none()
                && tab.editor.read(cx).value().trim().is_empty();
            if !reuse {
                self.add_tab(None, Some(session), window, cx);
            }
            let tab = &mut self.tabs[self.active];
            tab.title = query.name.clone().into();
            tab.saved_query = Some(query.id);
            let editor = tab.editor.clone();
            editor.update(cx, |editor, cx| {
                editor.set_value(query.sql.clone(), window, cx)
            });
        }
        self.focus_editor(window, cx);
        if run {
            self.run(RunScope::All, window, cx);
        }
        cx.notify();
    }

    fn delete_saved_query(
        &mut self,
        query: &SavedQuery,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let body = format!("“{}” will be removed from your saved queries.", query.name);
        let preview = query.sql.clone();
        let dialog = cx.new(|cx| {
            ConfirmDialog::new("Delete saved query?", body, &preview, None, window, cx)
                .action("Delete", true)
        });
        self.show_confirm(dialog, Pending::DeleteQuery(query.id), window, cx);
    }

    pub(crate) fn forget_query(&mut self, id: i64, cx: &mut Context<Self>) {
        if let Some(store) = &self.store
            && let Err(err) = store.delete_query(id)
        {
            self.notice = Some(format!("{err:#}").into());
        }
        for tab in &mut self.tabs {
            if tab.saved_query == Some(id) {
                tab.saved_query = None;
            }
        }
        self.reload_saved_queries(cx);
    }

    // ------------------------------------------------------------ panel

    pub(crate) fn render_saved_queries(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let panel = div().flex_1().flex().flex_col().min_h(px(0.)).child(
            div().px(px(10.)).pb(px(8.)).child(search_field(
                theme,
                &self.saved_filter,
                self.field_focus.saved,
            )),
        );
        if self.saved_queries.is_empty() {
            let filtering = !self.saved_filter.read(cx).value().trim().is_empty();
            return panel
                .child(empty_hint(
                    theme,
                    if filtering {
                        "No saved queries match"
                    } else {
                        "Press ⌘S in the editor to save a query here"
                    },
                ))
                .into_any_element();
        }
        let now = now_ms();
        let open: Option<i64> = self.active_tab().saved_query;
        let classic = self.classic();
        let entry = |query: &xenon_store::SavedQuery, nested: bool, cx: &mut Context<Self>| {
            let preview: String = query
                .sql
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(160)
                .collect();
            let (load, remove) = (query.clone(), query.clone());
            let active = open == Some(query.id);
            div()
                .id(SharedString::from(format!("saved-{}", query.id)))
                .group("saved-entry")
                .relative()
                .pl(px(if nested { 30. } else { 12. }))
                .pr(px(12.))
                .py(px(7.))
                .flex()
                .flex_col()
                .gap(px(3.))
                // Classic: rows split by hairlines. Floating: rounded, set in.
                .map(|el| {
                    if classic {
                        el.border_b_1().border_color(theme.line_2)
                    } else {
                        el.mx(px(6.))
                            .pl(px(if nested { 24. } else { 8. }))
                            .pr(px(8.))
                            .rounded(px(6.))
                    }
                })
                .cursor_pointer()
                .when(active, |el| el.bg(theme.acc_wash))
                .hover(|s| s.bg(theme.s3))
                .on_click(
                    cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                        // Click opens the query; double-click runs it.
                        this.open_saved_query(load.clone(), event.click_count() >= 2, window, cx);
                    }),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .truncate()
                                .text_size(px(12.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child(crate::folders::split(&query.name).1.to_string()),
                        )
                        .child(
                            div()
                                .font_family(MONO_FONT)
                                .text_size(px(10.))
                                .text_color(theme.t3)
                                .child(ago(now - query.updated_at)),
                        ),
                )
                .child(
                    div()
                        .font_family(MONO_FONT)
                        .text_size(px(10.5))
                        .text_color(theme.t3)
                        .truncate()
                        .child(preview),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("delete-saved-{}", query.id)))
                        .absolute()
                        .top(px(6.))
                        .right(px(6.))
                        .size(px(22.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .bg(theme.s2)
                        .invisible()
                        .group_hover("saved-entry", |el| el.visible())
                        .hover(|el| el.bg(theme.s3))
                        .tooltip(widgets::tooltip("Delete", ""))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.delete_saved_query(&remove, window, cx)
                        }))
                        .child(
                            svg()
                                .path(assets::icon("trash-2"))
                                .size(px(12.))
                                .text_color(theme.t2),
                        ),
                )
                .into_any_element()
        };
        let mut entries: Vec<AnyElement> = Vec::new();
        for (folder, members) in crate::folders::group(&self.saved_queries, |q| &q.name) {
            let nested = !folder.is_empty();
            if nested {
                let key = format!("queries/{folder}");
                let folded = self.folded_folders.contains(&key);
                entries.push(
                    self.render_folder(&folder, key, folded, members.len(), theme, cx)
                        .into_any_element(),
                );
                if folded {
                    continue;
                }
            }
            for query in members {
                entries.push(entry(query, nested, cx));
            }
        }
        panel
            .child(
                div()
                    .id("saved-list")
                    .flex_1()
                    .overflow_y_scroll()
                    .children(entries),
            )
            .into_any_element()
    }

    /// The "Save query" box: a name and Save.
    pub(crate) fn render_naming(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let naming = self.naming.as_ref()?;
        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_start()
                .justify_center()
                .pt(px(120.))
                .bg(theme.scrim)
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.cancel_naming(window, cx)),
                )
                .child(
                    widgets::overlay_panel(theme)
                        .key_context("SaveQuery")
                        .on_action(cx.listener(
                            |this, _: &gpui_component::input::Escape, window, cx| {
                                this.cancel_naming(window, cx)
                            },
                        ))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .w(px(420.))
                        .h_auto()
                        .p(px(16.))
                        .gap(px(12.))
                        .child(
                            div()
                                .text_size(px(14.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.text)
                                .child(if naming.copy {
                                    "Save a copy"
                                } else {
                                    "Save query"
                                }),
                        )
                        .child(widgets::text_field(theme, &naming.input, true))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .child(
                                    widgets::button(
                                        theme,
                                        "naming-cancel",
                                        "Cancel",
                                        ButtonKind::Ghost,
                                        true,
                                    )
                                    .on_click(cx.listener(
                                        |this, _, window, cx| this.cancel_naming(window, cx),
                                    )),
                                )
                                .child(
                                    widgets::button(
                                        theme,
                                        "naming-save",
                                        "Save",
                                        ButtonKind::Primary,
                                        true,
                                    )
                                    .on_click(cx.listener(
                                        |this, _, window, cx| this.finish_naming(window, cx),
                                    )),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    // ---------------------------------------------------------- restoring

    /// The tabs worth reopening: those on saved connections.
    fn open_tabs_snapshot(&self, cx: &Context<Self>) -> Vec<OpenTab> {
        self.tabs
            .iter()
            .enumerate()
            .filter_map(|(index, tab)| {
                let session = self.session(tab.session?)?;
                // Production SQL stays off disk when its history does.
                if !self.keeps_history(session) {
                    return None;
                }
                let connection = session.saved_id?;
                Some(OpenTab {
                    connection,
                    title: tab.title.to_string(),
                    sql: tab.editor.read(cx).value().to_string(),
                    table: tab
                        .browse
                        .as_ref()
                        .map(|b| (b.schema.clone(), b.table.clone())),
                    filter: tab
                        .browse
                        .as_ref()
                        .map(|b| b.filter.read(cx).value().to_string())
                        .unwrap_or_default(),
                    active: index == self.active,
                    saved_query: tab.saved_query,
                })
            })
            .collect()
    }

    /// Write the open tabs to the store when they've changed. Skipped while
    /// last session's tabs are still being reopened, so they aren't lost.
    pub(crate) fn persist_tabs(&mut self, cx: &Context<Self>) {
        if !self.restoring.is_empty() {
            return;
        }
        let tabs = if self.prefs.restore_tabs {
            self.open_tabs_snapshot(cx)
        } else {
            Vec::new()
        };
        if tabs == self.persisted_tabs {
            return;
        }
        if let Some(store) = &self.store
            && let Err(err) = store.set_open_tabs(&tabs)
        {
            eprintln!("xenon: couldn't remember open tabs: {err:#}");
        }
        self.persisted_tabs = tabs;
    }

    /// Save the tabs every few seconds, and when Xenon quits.
    pub(crate) fn watch_tabs(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(5))
                    .await;
                if this.update(cx, |this, cx| this.persist_tabs(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        let quit = cx.on_app_quit(|this, cx| {
            this.persist_tabs(cx);
            async {}
        });
        self._subscriptions.push(quit);
    }

    /// Reconnect to the saved connections last session's tabs were on; their
    /// tabs come back as each connects.
    pub(crate) fn restore_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(store) = &self.store else { return };
        let tabs = match store.open_tabs() {
            Ok(tabs) => tabs,
            Err(err) => {
                eprintln!("xenon: couldn't read last session's tabs: {err:#}");
                return;
            }
        };
        self.persisted_tabs = tabs.clone();
        let mut order: Vec<i64> = Vec::new();
        let mut by_connection: HashMap<i64, Vec<OpenTab>> = HashMap::new();
        for tab in tabs {
            if !self.saved.iter().any(|s| s.id == tab.connection) {
                continue;
            }
            if !order.contains(&tab.connection) {
                order.push(tab.connection);
            }
            by_connection.entry(tab.connection).or_default().push(tab);
        }
        self.restoring = by_connection;
        for id in order {
            if let Some(saved) = self.saved.iter().find(|s| s.id == id).cloned() {
                self.connect_to_saved(&saved, window, cx);
            }
        }
    }

    /// Reopen a connection's tabs from last session. Returns false when it had none.
    pub(crate) fn reopen_tabs(
        &mut self,
        saved_id: Option<i64>,
        session: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(tabs) = saved_id.and_then(|id| self.restoring.remove(&id)) else {
            return false;
        };
        let mut active = None;
        for tab in tabs {
            self.add_tab(tab.table.clone(), Some(session), window, cx);
            let index = self.active;
            let open = &mut self.tabs[index];
            open.saved_query = tab.saved_query;
            let editor = open.editor.clone();
            editor.update(cx, |editor, cx| {
                editor.set_value(tab.sql.clone(), window, cx)
            });
            match &open.browse {
                Some(browse) => {
                    let filter = browse.filter.clone();
                    filter.update(cx, |input, cx| {
                        input.set_value(tab.filter.clone(), window, cx)
                    });
                    self.load_table_info(cx);
                    self.run_browse(window, cx);
                }
                None => open.title = tab.title.clone().into(),
            }
            if tab.active {
                active = Some(index);
            }
        }
        // The empty Connect tab Xenon starts with isn't needed any more.
        if let Some(index) = self.tabs.iter().position(|t| t.session.is_none()) {
            self.tabs.remove(index);
            active = active.map(|a| if a > index { a - 1 } else { a });
            self.active = self.active.saturating_sub(usize::from(self.active > index));
        }
        if let Some(index) = active {
            self.active = index;
        }
        self.reload_history(cx);
        self.focus_editor(window, cx);
        cx.notify();
        true
    }
}
