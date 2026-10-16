//! Regression tests for the 0.5 hardening: each one failed before its fix.
//! They drive the real workspace headlessly, like the snapshots harness, and
//! run with it: `cargo test -p xenon_ui hardening -- --ignored`.

use std::path::{Path, PathBuf};

use xenon_store::{Environment, SavedConnection, Store, Target};

use crate::snapshots::{HARNESS, Shot, fixture};
use crate::theme::Mode;
use crate::workspace::{RunScope, Workspace};

/// A fresh data folder for one test, with the shop fixture in it.
fn setup(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/hardening")
        .join(name);
    let _ = std::fs::remove_dir_all(&root);
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    // SAFETY: tests here hold HARNESS, so no other test thread reads it now.
    unsafe { std::env::set_var("XENON_DATA_DIR", &data) };
    let shop = fixture(&data);
    (root, data, shop)
}

fn set_sql(
    w: &mut Workspace,
    sql: &str,
    window: &mut gpui::Window,
    cx: &mut gpui::Context<Workspace>,
) {
    let editor = w.active_tab().editor.clone();
    editor.update(cx, |editor, cx| editor.set_value(sql, window, cx));
}

fn saved(name: &str, environment: Environment, path: &Path) -> SavedConnection {
    SavedConnection {
        id: 0,
        name: name.into(),
        target: Target::Sqlite {
            path: path.display().to_string(),
        },
        environment,
        read_only: false,
        color: String::new(),
    }
}

/// W1: opening a saved production connection while a development session to
/// the same database is open gets its own session, with production's guards.
#[test]
#[ignore]
fn hardening_production_never_reuses_a_development_session() {
    let _harness = HARNESS.lock().unwrap_or_else(|e| e.into_inner());
    let (root, data, shop) = setup("w1");
    let store = Store::open(&data.join("xenon.db")).unwrap();
    // The same file named two ways (the store keeps one entry per path).
    let link = data.join("shop-link.db");
    std::os::unix::fs::symlink(&shop, &link).unwrap();
    let dev_id = store
        .save_connection(&saved("shop dev", Environment::Development, &shop))
        .unwrap();
    let prod_id = store
        .save_connection(&saved("shop prod", Environment::Production, &link))
        .unwrap();
    let all = store.connections().unwrap();
    let dev = all.iter().find(|s| s.id == dev_id).unwrap().clone();
    let prod = all.iter().find(|s| s.id == prod_id).unwrap().clone();

    let mut shot = Shot::new(Mode::Dark, None, &root);
    shot.act(|w, window, cx| w.connect_to_saved(&dev, window, cx));
    assert!(shot.settle(10, |w| w.sessions.len() == 1));
    shot.act(|w, window, cx| w.connect_to_saved(&prod, window, cx));
    assert!(shot.settle(10, |w| w.sessions.len() == 2));
    shot.act(|w, window, cx| {
        let session = w.active_session().unwrap();
        assert_eq!(session.name, "shop prod");
        assert_eq!(session.environment, Environment::Production);
        set_sql(w, "delete from orders where id = 1", window, cx);
        w.run(RunScope::All, window, cx);
        assert!(
            w.confirm.is_some(),
            "a production delete ran without asking"
        );
        w.confirm = None;
    });
}

/// W2: rows from other SQL run in a table tab aren't saved into the table.
#[test]
#[ignore]
fn hardening_other_sql_in_a_table_tab_isnt_editable() {
    let _harness = HARNESS.lock().unwrap_or_else(|e| e.into_inner());
    let (root, _, shop) = setup("w2");
    let mut shot = Shot::new(Mode::Dark, Some(&shop), &root);
    shot.act(|w, window, cx| {
        let id = w.sessions[0].id;
        w.browse(id, "main", "orders", window, cx)
    });
    assert!(shot.settle(10, |w| !w.is_running() && w.active_tab().grid.is_some()));
    shot.act(|w, _, _| assert!(w.edit_target().is_ok(), "the table itself is editable"));
    shot.act(|w, window, cx| {
        set_sql(w, "select * from customers", window, cx);
        w.run(RunScope::All, window, cx);
    });
    assert!(shot.settle(10, |w| !w.is_running()));
    shot.act(|w, _, _| {
        assert!(
            w.edit_target().is_err(),
            "customers' rows would be saved into orders"
        )
    });
}

/// E3/R3: while a save runs on a connection (a transaction), nothing else
/// runs on it, so another tab's statement can't land inside the save.
#[test]
#[ignore]
fn hardening_nothing_runs_on_a_connection_while_it_saves() {
    let _harness = HARNESS.lock().unwrap_or_else(|e| e.into_inner());
    let (root, _, shop) = setup("e3");
    let mut shot = Shot::new(Mode::Dark, Some(&shop), &root);
    shot.act(|w, window, cx| {
        let saving = w.active_tab().id;
        w.new_tab(&crate::NewTab, window, cx);
        // The first tab's save is in flight.
        w.tabs.iter_mut().find(|t| t.id == saving).unwrap().saving = true;
        set_sql(w, "commit", window, cx);
        w.run(RunScope::All, window, cx);
        assert!(!w.is_running(), "ran on a connection that's saving");
        let flash = w
            .active_tab()
            .flash
            .clone()
            .map(|(text, _)| text.to_string());
        assert!(
            flash
                .as_deref()
                .is_some_and(|f| f.contains("already running")),
            "{flash:?}"
        );
    });
}

/// C1: a URL pasted into the connect dialog doesn't undo the TLS mode picked
/// in the form (a new production connection's verify-full).
#[test]
#[ignore]
fn hardening_a_pasted_url_keeps_the_forms_tls() {
    let _harness = HARNESS.lock().unwrap_or_else(|e| e.into_inner());
    let (root, _, _) = setup("c1");
    let mut shot = Shot::new(Mode::Dark, None, &root);
    shot.act(|w, window, cx| {
        w.new_server_connection(xenon_store::ServerEngine::Postgres, window, cx)
    });
    shot.act(|w, window, cx| {
        let dialog = w.connect_dialog.as_ref().expect("dialog").0.clone();
        let request = dialog.update(cx, |dialog, cx| {
            dialog.fill_for_test(
                "postgres://app@db.example.com/app?sslmode=prefer",
                xenon_driver::SslMode::VerifyFull,
                window,
                cx,
            );
            dialog.request(cx)
        });
        let Some(crate::connect::Request {
            target: crate::connect::RequestTarget::Server(_, options),
            ..
        }) = request
        else {
            panic!("no server request")
        };
        assert_eq!(options.ssl, xenon_driver::SslMode::VerifyFull);
        // C2: a remote server without verify-full is warned about; a local one isn't.
        let remote = dialog.update(cx, |dialog, cx| {
            let remote = dialog.remote(cx);
            dialog.fill_for_test(
                "postgres://app@localhost/app",
                xenon_driver::SslMode::Prefer,
                window,
                cx,
            );
            (remote, dialog.remote(cx))
        });
        assert_eq!(remote, (true, false));
    });
}

/// W11: opening a NULL cell and leaving it empty keeps it NULL.
#[test]
#[ignore]
fn hardening_a_null_cell_left_empty_stays_null() {
    let _harness = HARNESS.lock().unwrap_or_else(|e| e.into_inner());
    let (root, _, shop) = setup("w11");
    let mut shot = Shot::new(Mode::Dark, Some(&shop), &root);
    shot.act(|w, window, cx| {
        let id = w.sessions[0].id;
        w.browse(id, "main", "orders", window, cx)
    });
    assert!(shot.settle(10, |w| !w.is_running() && w.active_tab().grid.is_some()));
    shot.act(|w, window, cx| {
        let grid = w.active_tab().grid.as_ref().unwrap();
        let note = grid
            .data
            .columns
            .iter()
            .position(|c| c.name == "note")
            .unwrap();
        assert!(
            grid.data.rows[0][note].is_null(),
            "the fixture's first note is NULL"
        );
        w.begin_edit(0, note, window, cx);
        w.commit_edit(false, window, cx);
        assert!(w.active_tab().edits.is_empty(), "NULL became ''");
    });
}

/// W12: closing a tab, disconnecting or quitting with staged changes asks
/// before discarding them.
#[test]
#[ignore]
fn hardening_unsaved_changes_are_never_dropped_silently() {
    let _harness = HARNESS.lock().unwrap_or_else(|e| e.into_inner());
    let (root, _, shop) = setup("w12");
    let mut shot = Shot::new(Mode::Dark, Some(&shop), &root);
    shot.act(|w, window, cx| {
        let id = w.sessions[0].id;
        w.browse(id, "main", "orders", window, cx)
    });
    assert!(shot.settle(10, |w| !w.is_running() && w.active_tab().grid.is_some()));
    shot.act(|w, window, cx| {
        let status = w
            .active_tab()
            .grid
            .as_ref()
            .unwrap()
            .data
            .columns
            .iter()
            .position(|c| c.name == "status")
            .unwrap();
        w.begin_edit(0, status, window, cx);
        w.type_into_cell("changed", window, cx);
        w.commit_edit(false, window, cx);
        assert!(!w.active_tab().edits.is_empty());
        let tabs = w.tabs().len();
        w.close_tab_at(w.active, window, cx);
        assert!(w.confirm.is_some(), "closing the tab didn't ask");
        assert_eq!(w.tabs().len(), tabs);
        w.confirm = None;
        let session = w.sessions[0].id;
        w.close_session(session, window, cx);
        assert!(w.confirm.is_some(), "disconnecting didn't ask");
        assert_eq!(w.sessions.len(), 1);
        w.confirm = None;
        assert!(!w.may_quit(window, cx), "quitting didn't ask");
        w.confirm = None;
    });
}

/// C7/W6/C10: a URL (pasted, launched or found on this Mac) for a saved
/// production database opens as that saved connection: its TLS and tunnel,
/// not the URL's, so its password never goes another way.
#[test]
#[ignore]
fn hardening_a_url_for_a_saved_database_opens_it_as_saved() {
    let _harness = HARNESS.lock().unwrap_or_else(|e| e.into_inner());
    let (root, data, shop) = setup("c7");
    crate::snapshots::seed_saved(&Store::open(&data.join("xenon.db")).unwrap(), &shop);
    let mut shot = Shot::new(Mode::Dark, None, &root);
    shot.act(|w, _, _| {
        let engine = xenon_store::ServerEngine::Postgres;
        let options = crate::servers::parse_url(
            engine,
            "postgres://app@db.billing.internal/billing?sslmode=disable",
        )
        .unwrap();
        let profile = w.profile_for_url(engine, options);
        assert!(
            profile.saved_id.is_some(),
            "not matched to the saved connection"
        );
        assert_eq!(profile.environment, Environment::Production);
        let crate::connect::RequestTarget::Server(_, target) = profile.target else {
            panic!("not a server")
        };
        assert_ne!(
            target.ssl,
            xenon_driver::SslMode::Disable,
            "the URL's TLS won"
        );
        assert!(target.ssh.is_some(), "the saved tunnel was dropped");
    });
}

/// W3: shortcuts typed inside a dialog don't act on the tab behind it.
#[test]
#[ignore]
fn hardening_shortcuts_in_a_dialog_stay_in_it() {
    let _harness = HARNESS.lock().unwrap_or_else(|e| e.into_inner());
    let (root, _, shop) = setup("w3");
    let mut shot = Shot::new(Mode::Dark, Some(&shop), &root);
    shot.act(|w, window, cx| {
        set_sql(
            w,
            "update orders set status = 'from-the-dialog' where id = 4",
            window,
            cx,
        );
        w.new_server_connection(xenon_store::ServerEngine::Postgres, window, cx);
        assert!(w.connect_dialog.is_some());
    });
    // Typed as keys, from the dialog's focused field.
    for keys in ["cmd-shift-enter", "cmd-enter", "cmd-w"] {
        shot.keys(keys);
    }
    assert!(shot.settle(5, |w| !w.is_running()));
    shot.act(|w, _, _| {
        assert_eq!(w.tabs().len(), 1, "⌘W closed the tab behind the dialog");
        let status = match w.sessions[0]
            .conn
            .execute("select status from orders where id = 4", 1)
            .unwrap()
            .outcome
        {
            xenon_driver::QueryOutcome::Rows(rows) => rows.rows[0][0].clone(),
            other => panic!("{other:?}"),
        };
        assert_ne!(status, xenon_driver::Value::Text("from-the-dialog".into()));
    });
}

/// W5: marking an open connection Production takes effect at once, on the
/// session already open, not at the next reconnect.
#[test]
#[ignore]
fn hardening_marking_an_open_connection_production_applies_now() {
    let _harness = HARNESS.lock().unwrap_or_else(|e| e.into_inner());
    let (root, data, shop) = setup("w5");
    let store = Store::open(&data.join("xenon.db")).unwrap();
    let id = store
        .save_connection(&saved("shop", Environment::Development, &shop))
        .unwrap();
    let dev = store
        .connections()
        .unwrap()
        .into_iter()
        .find(|s| s.id == id)
        .unwrap();
    let mut shot = Shot::new(Mode::Dark, None, &root);
    shot.act(|w, window, cx| w.connect_to_saved(&dev, window, cx));
    assert!(shot.settle(10, |w| w.sessions.len() == 1));
    // Edited in the connection form and saved: now Production.
    let production = SavedConnection {
        environment: Environment::Production,
        name: "shop PRODUCTION".into(),
        ..dev.clone()
    };
    store.save_connection(&production).unwrap();
    shot.act(|w, _, cx| {
        let session = w.sessions[0].id;
        w.update_open(
            session,
            crate::workspace::Profile::from_saved(&production),
            cx,
        );
    });
    assert!(shot.settle(5, |w| {
        w.active_session()
            .is_some_and(|s| s.name == "shop PRODUCTION")
    }));
    shot.act(|w, window, cx| {
        assert_eq!(w.sessions.len(), 1, "a second session was opened");
        let session = w.active_session().unwrap();
        assert_eq!(session.environment, Environment::Production);
        assert_eq!(session.name, "shop PRODUCTION");
        set_sql(w, "delete from orders where id = 1", window, cx);
        w.run(RunScope::All, window, cx);
        assert!(
            w.confirm.is_some(),
            "a delete ran without the production prompt"
        );
        w.confirm = None;
    });
    // Made read-only too: with nothing running, the session's connection is
    // swapped for a read-only one at once.
    let read_only = SavedConnection {
        read_only: true,
        ..production.clone()
    };
    store.save_connection(&read_only).unwrap();
    shot.act(|w, _, cx| {
        let session = w.sessions[0].id;
        w.update_open(
            session,
            crate::workspace::Profile::from_saved(&read_only),
            cx,
        );
    });
    assert!(shot.settle(5, |w| w.active_session().is_some_and(|s| s.read_only)));
    shot.act(|w, _, _| {
        assert!(
            w.sessions[0]
                .conn
                .execute("delete from orders where id = 1", 1)
                .is_err(),
            "the open session still writes"
        );
    });
}

/// W7: a history entry from another connection is loaded, not run, here.
#[test]
#[ignore]
fn hardening_history_runs_only_on_its_own_connection() {
    let _harness = HARNESS.lock().unwrap_or_else(|e| e.into_inner());
    let (root, _, shop) = setup("w7");
    let mut shot = Shot::new(Mode::Dark, Some(&shop), &root);
    shot.act(|w, window, cx| {
        w.use_history_entry(
            "update orders set status = 'from-history' where id = 5".into(),
            "postgres:someone@elsewhere:5432/prod",
            true,
            window,
            cx,
        );
        assert!(!w.is_running(), "ran another connection's statement here");
    });
    assert!(shot.settle(3, |w| !w.is_running()));
    shot.act(|w, _, _| {
        let status = match w.sessions[0]
            .conn
            .execute("select status from orders where id = 5", 1)
            .unwrap()
            .outcome
        {
            xenon_driver::QueryOutcome::Rows(rows) => rows.rows[0][0].clone(),
            other => panic!("{other:?}"),
        };
        assert_ne!(status, xenon_driver::Value::Text("from-history".into()));
    });
}
