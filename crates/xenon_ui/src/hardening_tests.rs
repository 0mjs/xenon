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
    });
}
