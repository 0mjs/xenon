//! Noticing a connection that went away (the Mac slept, the network dropped,
//! the server restarted) and opening it again with the same settings.
//!
//! Before running on a connection idle for a while, Xenon checks the server
//! still answers, and reconnects first if not. After a query fails with
//! anything but the server's own error, it checks again. A query is never run
//! again by itself: it may have been a write, or relied on a transaction the
//! dropped session took with it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context, SharedString};
use xenon_driver::{Connection, QueryError};

use crate::completion::Catalog;
use crate::workspace::{Profile, Workspace};

/// A connection unused for longer than this is checked before it runs anything.
const IDLE: Duration = Duration::from_secs(60);
/// How long the server has to answer before the connection counts as gone. A
/// dead network can leave a check waiting for minutes.
const ANSWER_WITHIN: Duration = Duration::from_secs(5);

enum Checked {
    Alive,
    Reconnected(Arc<dyn Connection>),
    Failed(anyhow::Error),
}

/// Whether `conn` answers within [`ANSWER_WITHIN`].
fn answers(conn: Arc<dyn Connection>) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    // Left to finish (or not) by itself if the network never answers.
    std::thread::spawn(move || {
        let _ = tx.send(conn.alive());
    });
    rx.recv_timeout(ANSWER_WITHIN).unwrap_or(false)
}

fn check(conn: Arc<dyn Connection>, profile: Profile) -> Checked {
    if answers(conn) {
        return Checked::Alive;
    }
    match profile.connect() {
        Ok(conn) => Checked::Reconnected(conn),
        Err(err) => Checked::Failed(err),
    }
}

/// Whether a failed query's error could mean the connection went away: not the
/// server refusing the statement, which proves it's there.
pub(crate) fn may_be_lost(err: &anyhow::Error) -> bool {
    !err.is::<QueryError>()
}

impl Workspace {
    /// Whether a connection has sat unused long enough to check before using it.
    pub(crate) fn needs_check(&self, session_id: usize) -> bool {
        self.session(session_id)
            .is_some_and(|s| !s.reconnecting && s.last_used.elapsed() >= IDLE)
    }

    /// Check a connection that's been idle, reconnecting if it's gone, then
    /// `then` (run the query that was waiting). If a transaction was open and
    /// went with the old session, nothing runs: the next statement assumed it.
    pub(crate) fn check_then(
        &mut self,
        session_id: usize,
        tab_id: usize,
        then: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session_mut(session_id) else {
            return;
        };
        session.reconnecting = true;
        let (conn, profile) = (session.conn.clone(), session.profile.clone());
        let task = cx.background_spawn(async move { check(conn, profile) });
        cx.spawn(async move |this, cx| {
            let checked = task.await;
            this.update(cx, |this, cx| {
                match checked {
                    Checked::Alive => {
                        if let Some(session) = this.session_mut(session_id) {
                            session.reconnecting = false;
                            session.last_used = Instant::now();
                        }
                        then(this, cx);
                    }
                    Checked::Reconnected(conn) => {
                        let (name, lost_transaction) = this.adopt(session_id, conn);
                        if lost_transaction {
                            this.tell(
                                tab_id,
                                format!(
                                    "Reconnected to {name}. Its open transaction was rolled \
                                     back when it dropped, so nothing ran."
                                ),
                                true,
                            );
                        } else {
                            then(this, cx);
                            this.tell(
                                tab_id,
                                format!("Reconnected to {name}: the connection had dropped."),
                                false,
                            );
                        }
                    }
                    Checked::Failed(err) => {
                        let name = this.lose(session_id);
                        this.tell(
                            tab_id,
                            format!(
                                "Lost the connection to {name} and couldn't reconnect: {err:#}"
                            ),
                            true,
                        );
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A query failed in a way that could mean the connection dropped: check,
    /// and reconnect if so. The query isn't run again.
    pub(crate) fn check_after_failure(
        &mut self,
        session_id: usize,
        tab_id: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session_mut(session_id) else {
            return;
        };
        if session.reconnecting {
            return;
        }
        session.reconnecting = true;
        let (conn, profile) = (session.conn.clone(), session.profile.clone());
        let task = cx.background_spawn(async move { check(conn, profile) });
        cx.spawn(async move |this, cx| {
            let checked = task.await;
            this.update(cx, |this, cx| {
                match checked {
                    Checked::Alive => {
                        if let Some(session) = this.session_mut(session_id) {
                            session.reconnecting = false;
                        }
                    }
                    Checked::Reconnected(conn) => {
                        let (name, was_open) = this.adopt(session_id, conn);
                        let text = if was_open {
                            format!(
                                "Reconnected to {name}. It dropped mid-query; its open \
                                 transaction was rolled back."
                            )
                        } else {
                            format!(
                                "Reconnected to {name}. It dropped mid-query, so this may \
                                 not have run."
                            )
                        };
                        this.tell(tab_id, text, true);
                    }
                    Checked::Failed(err) => {
                        let name = this.lose(session_id);
                        this.tell(
                            tab_id,
                            format!(
                                "The connection to {name} dropped, and reconnecting failed: {err:#}"
                            ),
                            true,
                        );
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Put a new connection in place of a session's dropped one, keeping its
    /// tabs, staged edits and explorer. Returns its name, and whether a
    /// transaction was open (and so lost).
    fn adopt(&mut self, session_id: usize, conn: Arc<dyn Connection>) -> (String, bool) {
        let Some(session) = self.session_mut(session_id) else {
            return (String::new(), false);
        };
        let lost_transaction = session.in_transaction;
        session.version = conn.server_version();
        // Autocomplete loads columns through the connection: give it the new one.
        *session.catalog.borrow_mut() = Some(std::rc::Rc::new(Catalog::new(
            session.schemas.clone(),
            conn.clone(),
        )));
        session.conn = conn;
        session.in_transaction = false;
        session.reconnecting = false;
        session.last_used = Instant::now();
        (session.name.clone(), lost_transaction)
    }

    /// Reconnecting failed: the next use checks again.
    fn lose(&mut self, session_id: usize) -> String {
        let Some(session) = self.session_mut(session_id) else {
            return String::new();
        };
        session.reconnecting = false;
        session.name.clone()
    }

    fn tell(&mut self, tab_id: usize, text: String, error: bool) {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
            tab.flash = Some((SharedString::from(text), error));
        }
    }
}
