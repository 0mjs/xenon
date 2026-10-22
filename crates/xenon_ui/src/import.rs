//! File → Import Connections…: paste connection URLs (or `DATABASE_URL=`
//! lines, or database file paths), one per line, and each becomes a saved
//! connection. Passwords go to the Keychain, as when connecting.

use std::path::Path;

use gpui::{AnyElement, Context, Entity, FontWeight, SharedString, Window, div, prelude::*, px};
use gpui_component::input::{Editor, EditorState};
use xenon_store::{Environment, SavedConnection, Target, secrets};

use crate::detect::{Detected, detect};
use crate::servers;
use crate::theme::Theme;
use crate::widgets::{self, ButtonKind};
use crate::workspace::Workspace;

/// What a pasted list would import.
#[derive(Default)]
pub(crate) struct Plan {
    /// New connections, with the password their URL carried.
    pub new: Vec<(SavedConnection, String)>,
    /// Lines for connections already saved.
    pub duplicates: usize,
    /// Line numbers (from 1) that weren't recognised.
    pub unrecognised: Vec<usize>,
}

// By hand: the passwords stay out of it.
impl std::fmt::Debug for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let new: Vec<&SavedConnection> = self.new.iter().map(|(saved, _)| saved).collect();
        f.debug_struct("Plan")
            .field("new", &new)
            .field("duplicates", &self.duplicates)
            .field("unrecognised", &self.unrecognised)
            .finish()
    }
}

/// Read a pasted list against the connections already saved.
pub(crate) fn plan(text: &str, saved: &[SavedConnection]) -> Plan {
    let mut plan = Plan::default();
    for (ix, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (target, name, password) = match detect(line) {
            Detected::Server(engine, url) => match servers::parse_url(engine, &url) {
                Ok(options) => (
                    Target::Server {
                        engine,
                        host: options.host.clone(),
                        port: options.port,
                        database: options.database.clone(),
                        user: options.user.clone(),
                        ssl: options.ssl.as_str().to_string(),
                        ssh: None,
                        tls: servers::tls_paths(&options.tls),
                    },
                    servers::default_name(engine, &options),
                    options.password.clone(),
                ),
                Err(_) => {
                    plan.unrecognised.push(ix + 1);
                    continue;
                }
            },
            Detected::File(path) => (
                Target::Sqlite {
                    path: path.display().to_string(),
                },
                Path::new(&path).file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into(),
                ),
                String::new(),
            ),
            Detected::NotYet(_) | Detected::Unknown => {
                plan.unrecognised.push(ix + 1);
                continue;
            }
        };
        let known = saved.iter().any(|s| s.target == target)
            || plan.new.iter().any(|(s, _)| s.target == target);
        if known {
            plan.duplicates += 1;
            continue;
        }
        plan.new.push((
            SavedConnection {
                id: 0,
                name,
                target,
                environment: Environment::None,
                read_only: false,
                color: String::new(),
            },
            password,
        ));
    }
    plan
}

/// Waiting for a list to import.
pub(crate) struct Import {
    pub(crate) editor: Entity<EditorState>,
    note: Option<(SharedString, bool)>,
}

impl Workspace {
    pub(crate) fn open_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx).line_number(true).placeholder(
                "postgres://app@db.acme.dev/app\nmysql://reports@10.0.0.4/sales\n~/data/local.db",
            )
        });
        editor.update(cx, |editor, cx| editor.focus(window, cx));
        self.import = Some(Import { editor, note: None });
        cx.notify();
    }

    pub(crate) fn finish_import(&mut self, cx: &mut Context<Self>) {
        let Some(import) = &mut self.import else {
            return;
        };
        let text = import.editor.read(cx).value().to_string();
        let plan = plan(&text, &self.saved);
        if plan.new.is_empty() {
            import.note = Some((
                match (plan.duplicates, plan.unrecognised.len()) {
                    (0, 0) => "Paste one connection URL or file path per line.".into(),
                    (0, _) => unrecognised(&plan.unrecognised).into(),
                    _ => "Those are all saved already.".into(),
                },
                true,
            ));
            cx.notify();
            return;
        }
        let Some(store) = self.store.clone() else {
            return;
        };
        let task = cx.background_spawn(async move {
            let mut imported = 0;
            for (saved, password) in &plan.new {
                let id = store.save_connection(saved)?;
                if !password.is_empty() {
                    secrets::set(&SavedConnection::secret_key(id), password)?;
                }
                imported += 1;
            }
            anyhow::Ok((imported, plan.duplicates, plan.unrecognised))
        });
        cx.spawn(async move |this, cx| {
            let done = task.await;
            this.update(cx, |this, cx| {
                this.reload_saved();
                match done {
                    Ok((imported, duplicates, skipped)) => {
                        let mut text = format!(
                            "Imported {imported} connection{}.",
                            if imported == 1 { "" } else { "s" }
                        );
                        if duplicates > 0 {
                            text.push_str(&format!(" {duplicates} already saved."));
                        }
                        if !skipped.is_empty() {
                            text.push(' ');
                            text.push_str(&unrecognised(&skipped));
                        }
                        this.import = None;
                        this.notice = Some(text.into());
                    }
                    Err(err) => {
                        if let Some(import) = &mut this.import {
                            import.note = Some((format!("{err:#}").into(), true));
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn render_import(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let import = self.import.as_ref()?;
        let close = |this: &mut Self, cx: &mut Context<Self>| {
            this.import = None;
            cx.notify();
        };
        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_start()
                .justify_center()
                .pt(px(100.))
                .bg(theme.scrim)
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _, _, cx| close(this, cx)),
                )
                .child(
                    widgets::overlay_panel(theme)
                        .key_context("Import")
                        .on_action(cx.listener(
                            move |this, _: &gpui_component::input::Escape, _, cx| close(this, cx),
                        ))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .w(px(560.))
                        .h_auto()
                        .p(px(16.))
                        .gap(px(12.))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(
                                    div()
                                        .text_size(crate::theme::text(14.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("Import connections"),
                                )
                                .child(
                                    div()
                                        .text_size(crate::theme::text(11.5))
                                        .text_color(theme.t3)
                                        .child(
                                            "One per line: postgres://, mysql://, mariadb:// or \
                                     sqlserver:// URLs, DATABASE_URL= lines, or database file \
                                     paths. Passwords in them go to the Keychain.",
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .h(px(200.))
                                .rounded(px(widgets::RADIUS_CONTROL))
                                .border_1()
                                .border_color(theme.line)
                                .bg(theme.field)
                                .p(px(6.))
                                .font_family(crate::theme::mono_font())
                                .text_size(crate::theme::text(11.5))
                                .child(Editor::new(&import.editor).h_full().appearance(false)),
                        )
                        .children(import.note.as_ref().map(|(text, error)| {
                            div()
                                .text_size(crate::theme::text(11.5))
                                .text_color(if *error { theme.err } else { theme.t2 })
                                .child(text.clone())
                        }))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .child(
                                    widgets::button(
                                        theme,
                                        "import-cancel",
                                        "Cancel",
                                        ButtonKind::Ghost,
                                        true,
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| close(this, cx))),
                                )
                                .child(
                                    widgets::button(
                                        theme,
                                        "import-run",
                                        "Import",
                                        ButtonKind::Primary,
                                        true,
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| this.finish_import(cx))),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }
}

fn unrecognised(lines: &[usize]) -> String {
    let list: Vec<String> = lines.iter().take(5).map(|n| n.to_string()).collect();
    let more = if lines.len() > 5 { ", …" } else { "" };
    format!(
        "Line{} {}{more} not recognised.",
        if lines.len() == 1 { "" } else { "s" },
        list.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pasted_list_becomes_connections() {
        let already = plan("postgres://app@db.acme.dev:5432/app", &[]).new;
        let saved: Vec<SavedConnection> = already.into_iter().map(|(s, _)| s).collect();
        let plan = plan(
            "postgres://app@db.acme.dev:5432/app\n\
             \n\
             # a comment\n\
             mysql://reports:s3cret@10.0.0.4/sales\n\
             DATABASE_URL=postgres://ci@localhost/test\n\
             not a connection\n\
             mysql://reports:s3cret@10.0.0.4/sales",
            &saved,
        );
        let names: Vec<_> = plan.new.iter().map(|(s, _)| s.name.as_str()).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        // The password goes to the Keychain, not the saved connection.
        assert_eq!(plan.new[0].1, "s3cret");
        // Already saved (first line), and repeated in the list (last line).
        assert_eq!(plan.duplicates, 2);
        assert_eq!(plan.unrecognised, [6]);
        assert_eq!(unrecognised(&plan.unrecognised), "Line 6 not recognised.");
    }
}
