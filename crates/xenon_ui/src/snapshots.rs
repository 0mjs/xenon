//! Renders Xenon's screens to PNGs without showing a window, for design review:
//!
//! ```sh
//! cargo test -p xenon_ui snapshots -- --ignored --nocapture
//! ```
//!
//! Images go to `target/snapshots/`. Uses a throwaway data directory and a small
//! fixture database, so it never touches real connections, history or the Keychain.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{AnyWindowHandle, AppContext as _, Entity, HeadlessAppContext, Window, px, size};
use gpui_component::Root;
use xenon_store::{Environment, SavedConnection, Store, Target};

use crate::theme::Mode;
use crate::workspace::{RunScope, Workspace};

const WIDTH: f32 = 1440.;
const HEIGHT: f32 = 900.;

struct Shot {
    cx: HeadlessAppContext,
    window: AnyWindowHandle,
    workspace: Entity<Workspace>,
    out: PathBuf,
}

impl Shot {
    fn new(mode: Mode, open: Option<&Path>, out: &Path) -> Self {
        let data = out.join("data/xenon.db");
        let value = if mode == Mode::Dark { "dark" } else { "light" };
        Store::open(&data)
            .unwrap()
            .set_setting("theme", value)
            .unwrap();
        let mut cx = HeadlessAppContext::with_platform(
            Arc::new(gpui_apple::AppleTextSystem::new()),
            Arc::new(crate::assets::Assets),
            gpui_platform::current_headless_renderer,
        );
        cx.update(|cx| {
            crate::fonts::load(cx).expect("fonts");
            crate::init(cx);
        });
        let slot: Rc<RefCell<Option<Entity<Workspace>>>> = Rc::default();
        let launch = open.map(|path| crate::Launch::Sqlite(path.to_path_buf()));
        let window = cx
            .open_window(size(px(WIDTH), px(HEIGHT)), {
                let slot = slot.clone();
                move |window, cx| {
                    let workspace = cx.new(|cx| Workspace::new(launch, window, cx));
                    *slot.borrow_mut() = Some(workspace.clone());
                    cx.new(|cx| Root::new(workspace, window, cx))
                }
            })
            .expect("open window");
        cx.run_until_parked();
        let workspace = slot.borrow().clone().expect("workspace");
        Self {
            cx,
            window: window.into(),
            workspace,
            out: out.to_path_buf(),
        }
    }

    fn act(&mut self, f: impl FnOnce(&mut Workspace, &mut Window, &mut gpui::Context<Workspace>)) {
        let workspace = self.workspace.clone();
        self.cx
            .update_window(self.window, |_, window, cx| {
                workspace.update(cx, |workspace, cx| f(workspace, window, cx));
            })
            .expect("update window");
        self.cx.run_until_parked();
    }

    /// Let background work and timers run until `done`, for up to `seconds`.
    fn settle(&mut self, seconds: u64, done: impl Fn(&Workspace) -> bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
        while std::time::Instant::now() < deadline {
            self.cx.run_until_parked();
            if self.cx.read_entity(&self.workspace, |w, _| done(w)) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    }

    fn save(&mut self, name: &str) {
        self.cx
            .update_window(self.window, |_, window, _| window.refresh())
            .expect("refresh");
        self.cx.run_until_parked();
        let image = self.cx.capture_screenshot(self.window).expect("render");
        let path = self.out.join(format!("{name}.png"));
        image.save(&path).expect("write png");
        println!("wrote {}", path.display());
    }
}

/// A second database, so screens can show several connections at once.
fn analytics(dir: &Path) -> PathBuf {
    let path = dir.join("analytics.db");
    let _ = std::fs::remove_file(&path);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(
        "create table events (id integer primary key, name text, user_id integer, at text);
         create table sessions (id integer primary key, user_id integer, started text, seconds integer);
         insert into events (name, user_id, at) values ('signup', 1, '2026-10-01'), ('login', 1, '2026-10-02');",
    )
    .unwrap();
    path
}

/// A small, realistic database for the screens that show data.
fn fixture(dir: &Path) -> PathBuf {
    let path = dir.join("shop.db");
    let _ = std::fs::remove_file(&path);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(
        "create table customers (id integer primary key, email text not null, name text, plan text, created_at text);
         create table orders (id integer primary key, customer_id integer not null references customers(id),
                              total numeric, status text, items integer, note text, created_at text);
         create view paid_orders as select * from orders where status = 'paid';",
    )
    .unwrap();
    let names = [
        "Aoife Byrne",
        "Liam Walsh",
        "Priya Nair",
        "Tom Okafor",
        "Mei Chen",
        "Ana Silva",
        "Jonas Berg",
    ];
    let plans = ["free", "pro", "team", "pro", "free"];
    for i in 1..=240 {
        let name = names[i % names.len()];
        db.execute(
            "insert into customers values (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                i as i64,
                format!(
                    "{}.{}@example.com",
                    name.split(' ').next().unwrap().to_lowercase(),
                    i
                ),
                if i % 11 == 0 { None } else { Some(name) },
                plans[i % plans.len()],
                format!("2026-0{}-{:02} 09:{:02}:00", 1 + i % 9, 1 + i % 27, i % 60)
            ],
        )
        .unwrap();
    }
    let statuses = ["paid", "paid", "refunded", "pending", "paid"];
    for i in 1..=600 {
        db.execute(
            "insert into orders values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                i as i64,
                (1 + (i * 7) % 240) as i64,
                ((i * 3797) % 90_000) as f64 / 100. + 3.5,
                statuses[i % statuses.len()],
                (1 + i % 6) as i64,
                if i % 17 == 0 {
                    Some("{\"gift\": true, \"wrap\": \"blue\"}")
                } else {
                    None
                },
                format!(
                    "2026-{:02}-{:02} {:02}:{:02}:00",
                    1 + i % 10,
                    1 + i % 27,
                    8 + i % 10,
                    i % 60
                )
            ],
        )
        .unwrap();
    }
    path
}

fn seed_saved(store: &Store, shop: &Path) {
    let pg = |name: &str, host: &str, db: &str, environment| SavedConnection {
        id: 0,
        name: name.into(),
        target: Target::Server {
            engine: xenon_store::ServerEngine::Postgres,
            host: host.into(),
            port: Some(5432),
            database: db.into(),
            user: "app".into(),
            ssl: "require".into(),
            ssh: (name == "Billing").then(|| xenon_store::SshTunnel {
                host: "bastion.billing.internal".into(),
                ..Default::default()
            }),
        },
        environment,
        read_only: false,
    };
    store
        .save_connection(&pg(
            "Billing",
            "db.billing.internal",
            "billing",
            Environment::Production,
        ))
        .unwrap();
    store
        .save_connection(&SavedConnection {
            read_only: true,
            ..pg(
                "Analytics replica",
                "replica.analytics.internal",
                "warehouse",
                Environment::Production,
            )
        })
        .unwrap();
    store
        .save_connection(&pg(
            "Staging",
            "staging.internal",
            "app",
            Environment::Development,
        ))
        .unwrap();
    store
        .save_connection(&SavedConnection {
            id: 0,
            name: "shop.db".into(),
            target: Target::Sqlite {
                path: shop.display().to_string(),
            },
            environment: Environment::None,
            read_only: false,
        })
        .unwrap();
}

#[test]
#[ignore]
fn snapshots() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let _ = std::fs::remove_file(data.join("xenon.db"));
    // SAFETY: set before any other thread reads the environment.
    unsafe { std::env::set_var("XENON_DATA_DIR", &data) };
    let shop = fixture(&data);
    let second = analytics(&data);
    seed_saved(&Store::open(&data.join("xenon.db")).unwrap(), &shop);

    for (mode, suffix) in [(Mode::Dark, "dark"), (Mode::Light, "light")] {
        let store = Store::open(&data.join("xenon.db")).unwrap();
        store.set_open_tabs(&[]).unwrap();
        let mut shot = Shot::new(mode, None, &root);
        shot.save(&format!("01-connect-{suffix}"));
        shot.act(|w, window, cx| w.new_connection(&crate::NewConnection, window, cx));
        shot.save(&format!("02-connect-dialog-{suffix}"));

        // A CSV file opens through DuckDB as a view named after it.
        let csv = data.join("signups.csv");
        std::fs::write(
            &csv,
            "id,email,plan,signed_up,mrr\n1,ada@example.com,team,2026-01-04,49.00\n\
             2,grace@example.com,pro,2026-02-11,19.00\n3,linus@example.com,team,2026-02-28,49.00\n\
             4,margaret@example.com,free,2026-03-15,0.00\n",
        )
        .unwrap();
        let mut shot = Shot::new(mode, Some(&csv), &root);
        shot.act(|w, window, cx| {
            let id = w.sessions[0].id;
            w.browse(id, "main", "signups", window, cx)
        });
        shot.save(&format!("27-duckdb-csv-{suffix}"));

        // Last session's tabs come back: a query and a filtered table on shop.db.
        let shop_id = store
            .connections()
            .unwrap()
            .into_iter()
            .find(|c| matches!(&c.target, Target::Sqlite { path } if path.ends_with("shop.db")))
            .map(|c| c.id);
        if let Some(connection) = shop_id {
            store
                .set_open_tabs(&[
                    xenon_store::OpenTab {
                        connection,
                        title: "Big orders".into(),
                        sql: "select * from orders where total > 500".into(),
                        ..Default::default()
                    },
                    xenon_store::OpenTab {
                        connection,
                        title: "customers".into(),
                        sql: String::new(),
                        table: Some(("main".into(), "customers".into())),
                        filter: "plan = 'team'".into(),
                        active: true,
                        saved_query: None,
                    },
                ])
                .unwrap();
            let mut shot = Shot::new(mode, None, &root);
            shot.save(&format!("26-restored-{suffix}"));
            store.set_open_tabs(&[]).unwrap();
        }

        let mut shot = Shot::new(mode, Some(&shop), &root);
        shot.act(|w, window, cx| {
            let id = w.sessions[0].id;
            w.browse(id, "main", "orders", window, cx)
        });
        shot.act(|w, window, cx| {
            w.select_cell(2, 2, false, window, cx);
            w.select_cell(5, 4, true, window, cx);
        });
        shot.save(&format!("03-browse-{suffix}"));
        shot.act(|w, _, cx| {
            w.export_menu = true;
            cx.notify();
        });
        shot.save(&format!("21-export-{suffix}"));
        shot.act(|w, _, cx| {
            w.export_menu = false;
            cx.notify();
        });
        // Structure, then back to the rows and through a foreign key.
        shot.act(|w, _, cx| w.toggle_structure(cx));
        shot.save(&format!("22-structure-{suffix}"));
        shot.act(|w, _, cx| w.toggle_structure(cx));
        shot.act(|w, window, cx| w.follow_key(2, 1, window, cx));
        shot.save(&format!("23-follow-{suffix}"));
        shot.act(|w, window, cx| {
            let id = w.sessions[0].id;
            w.browse(id, "main", "orders", window, cx)
        });

        // Editing: change a status, delete a row, add one; review; save.
        shot.act(|w, window, cx| {
            w.begin_edit(1, 3, window, cx);
            w.type_into_cell("shipped", window, cx);
            w.commit_edit(false, window, cx);
            w.select_cell(4, 0, false, window, cx);
            w.delete_rows(&crate::DeleteRows, window, cx);
            w.add_row(&crate::AddRow, window, cx);
            let last = w.last_row();
            w.begin_edit(last, 1, window, cx);
            w.type_into_cell("42", window, cx);
            w.commit_edit(false, window, cx);
            w.begin_edit(7, 5, window, cx);
        });
        shot.save(&format!("17-editing-{suffix}"));
        shot.act(|w, window, cx| {
            w.commit_edit(true, window, cx);
            w.save_edits(&crate::SaveEdits, window, cx);
        });
        shot.save(&format!("18-save-review-{suffix}"));
        shot.act(|w, window, cx| {
            w.confirm = None;
            w.prefs.confirm_risky = false;
            w.save_edits(&crate::SaveEdits, window, cx);
        });
        shot.save(&format!("19-saved-{suffix}"));

        // Value editors: JSON in a text area, true / false, a date with Now.
        shot.act(|w, _, cx| {
            w.naming = None;
            // Through Xenon, so it sees the new table.
            let tab = w.active_tab().id;
            w.execute(
                tab,
                "drop table if exists profiles;
                 create table profiles (id integer primary key, settings json, active boolean, joined date);
                 insert into profiles values
                   (1, '{\"theme\":\"dark\",\"alerts\":{\"email\":true,\"sms\":false},\"tags\":[\"beta\",\"vip\"]}', 1, '2026-03-14')"
                    .into(),
                None,
                cx,
            );
        });
        shot.act(|w, window, cx| {
            let id = w.sessions[0].id;
            w.browse(id, "main", "profiles", window, cx);
        });
        shot.act(|w, window, cx| w.begin_edit(0, 1, window, cx));
        shot.save(&format!("31-json-editor-{suffix}"));
        shot.act(|w, window, cx| {
            // Invalid JSON is refused, and the editor stays open to fix it.
            w.type_into_area("{\"theme\": ", window, cx);
            w.commit_edit(false, window, cx);
            assert!(w.is_editing(), "invalid JSON was staged");
            let note = w.flash_text().unwrap_or_default();
            assert!(note.starts_with("Not valid JSON"), "{note}");
        });
        shot.act(|w, window, cx| {
            w.cancel_edit_for_test(window, cx);
            w.begin_edit(0, 2, window, cx);
        });
        shot.save(&format!("32-bool-editor-{suffix}"));
        shot.act(|w, window, cx| {
            // Picking a value stages it.
            w.commit_text("0".into(), window, cx);
            assert_eq!(w.staged_changes(), 1);
            w.discard_edits(&crate::DiscardEdits, window, cx);
        });
        shot.act(|w, window, cx| {
            w.cancel_edit_for_test(window, cx);
            w.begin_edit(0, 3, window, cx);
        });
        shot.save(&format!("33-date-editor-{suffix}"));
        shot.act(|w, window, cx| {
            w.cancel_edit_for_test(window, cx);
            let id = w.sessions[0].id;
            w.browse(id, "main", "orders", window, cx);
        });

        // Autocomplete: columns of the aliased table after `o.`.
        shot.act(|w, window, cx| {
            let editor = w.active_tab().editor.clone();
            editor.update(cx, |editor, cx| {
                editor.set_value(
                    "select o.st\nfrom orders o\njoin customers c on c.id = o.customer_id",
                    window,
                    cx,
                );
                editor.set_selected_range(11..11, cx);
                editor.focus(window, cx);
                editor.show_completions(window, cx);
            });
        });
        shot.save(&format!("20-autocomplete-{suffix}"));
        // Save a query: name it, then see it in the Saved panel.
        shot.act(|w, window, cx| {
            w.new_tab(&crate::NewTab, window, cx);
            let editor = w.active_tab().editor.clone();
            editor.update(cx, |editor, cx| {
                editor.set_value(
                    "select status, count(*) from orders group by status",
                    window,
                    cx,
                )
            });
            w.run(crate::workspace::RunScope::All, window, cx);
        });
        shot.act(|w, window, cx| w.save_query(false, window, cx));
        shot.act(|w, window, cx| {
            let input = w.naming.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| {
                input.set_value("Orders by status", window, cx)
            });
        });
        shot.save(&format!("24-save-query-{suffix}"));
        shot.act(|w, window, cx| {
            w.finish_naming(window, cx);
            w.show_panel(crate::workspace::SidePanel::Saved, window, cx);
        });
        shot.save(&format!("25-saved-queries-{suffix}"));
        shot.act(|w, window, cx| w.show_panel(crate::workspace::SidePanel::Explorer, window, cx));

        // A transaction left open shows in the status bar, with Commit and Roll back.
        shot.act(|w, window, cx| {
            let editor = w.active_tab().editor.clone();
            editor.update(cx, |editor, cx| {
                editor.set_value(
                    "begin;\nupdate orders set status = 'held' where id = 2",
                    window,
                    cx,
                )
            });
            w.run(crate::workspace::RunScope::All, window, cx);
        });
        shot.save(&format!("28-transaction-open-{suffix}"));
        // Quitting now asks, since quitting would roll it back.
        shot.act(|w, window, cx| assert!(!w.may_quit(window, cx)));
        shot.save(&format!("30-quit-with-transaction-{suffix}"));
        shot.act(|w, _, cx| {
            w.confirm = None;
            cx.notify();
        });
        shot.act(|w, _, cx| {
            assert!(w.active_session().unwrap().in_transaction);
            w.end_transaction(false, cx);
        });
        shot.act(|w, _, _| assert!(!w.active_session().unwrap().in_transaction));

        // On production, a script that fails part-way keeps none of its changes.
        shot.act(|w, window, cx| {
            w.sessions[0].environment = xenon_store::Environment::Production;
            let tab = w.active_tab().id;
            w.execute(
                tab,
                "update orders set status = 'lost' where id = 1;\ninsert into no_such_table values (1)"
                    .into(),
                None,
                cx,
            );
            let _ = window;
        });
        shot.act(|w, _, _| {
            // It ran, failed, and was rolled back…
            let error = w.error_text().unwrap_or_default();
            assert!(error.contains("nothing it did was kept"), "{error}");
            // …so the first statement's change isn't there.
            let status = w.sessions[0]
                .conn
                .execute("select status from orders where id = 1", 1)
                .unwrap();
            let xenon_driver::QueryOutcome::Rows(rows) = status.outcome else {
                panic!("no rows")
            };
            assert_ne!(rows.rows[0][0], xenon_driver::Value::Text("lost".into()));
        });
        // On production a write asks first, and so does Fetch all re-running one.
        shot.act(|w, window, cx| {
            let editor = w.active_tab().editor.clone();
            editor.update(cx, |editor, cx| {
                editor.set_value("delete from orders where id > 590 returning id", window, cx)
            });
            w.run(crate::workspace::RunScope::All, window, cx);
            assert!(w.confirm.is_some(), "a production write ran without asking");
            w.confirm = None;
            w.prefs.row_limit = 2;
            let tab = w.active_tab().id;
            w.execute(
                tab,
                "delete from orders where id > 590 returning id".into(),
                None,
                cx,
            );
        });
        shot.act(|w, window, cx| {
            w.fetch_all(window, cx);
            assert!(
                w.confirm.is_some(),
                "Fetch all re-ran a production write without asking"
            );
            w.confirm = None;
            w.prefs.row_limit = 100_000;
        });
        shot.save(&format!("29-script-rolled-back-{suffix}"));
        shot.act(|w, _, _| w.sessions[0].environment = xenon_store::Environment::None);

        // One query at a time per connection: a second tab on it is turned away.
        // Seconds of work for SQLite: long enough to still be running when checked.
        const SLOW: &str = "with recursive c(x) as (select 1 union all select x + 1 from c \
                            where x < 300000000) select count(*) from c";
        const BRIEF: &str = "with recursive c(x) as (select 1 union all select x + 1 from c \
                             where x < 20000000) select count(*) from c";
        shot.act(|w, window, cx| {
            let first = w.active_tab().id;
            w.execute(first, BRIEF.into(), None, cx);
            w.new_tab(&crate::NewTab, window, cx);
            let editor = w.active_tab().editor.clone();
            editor.update(cx, |editor, cx| editor.set_value("select 1", window, cx));
            w.run(crate::workspace::RunScope::All, window, cx);
            assert!(!w.is_running(), "a second query ran on a busy connection");
            let note = w.flash_text().unwrap_or_default();
            assert!(note.contains("already running"), "{note}");
            w.close_tab(&crate::CloseTab, window, cx);
            w.cancel_running(first);
        });
        assert!(
            shot.settle(10, |w| !w.any_running()),
            "the slow query didn't stop"
        );

        // The production time limit stops a long query and says why.
        shot.act(|w, _, cx| {
            w.sessions[0].environment = xenon_store::Environment::Production;
            w.prefs.query_limit = 1;
            let tab = w.active_tab().id;
            w.execute(tab, SLOW.into(), None, cx);
        });
        assert!(
            shot.settle(15, |w| !w.any_running()),
            "the time limit didn't stop the query"
        );
        shot.act(|w, _, _| {
            let error = w.error_text().unwrap_or_default();
            assert!(error.contains("time limit"), "{error}");
            w.prefs.query_limit = 60;
            w.sessions[0].environment = xenon_store::Environment::None;
        });

        // A pasted URL for a saved production database keeps its guards.
        shot.act(|w, _, _| {
            let billing = crate::servers::parse_url(
                xenon_store::ServerEngine::Postgres,
                "postgres://app:secret@db.billing.internal/billing",
            )
            .unwrap();
            let saved = w.saved_for(xenon_store::ServerEngine::Postgres, &billing);
            assert_eq!(
                saved.map(|s| s.environment),
                Some(xenon_store::Environment::Production)
            );
            let other = crate::servers::parse_url(
                xenon_store::ServerEngine::Postgres,
                "postgres://app:secret@db.billing.internal/elsewhere",
            )
            .unwrap();
            assert!(
                w.saved_for(xenon_store::ServerEngine::Postgres, &other)
                    .is_none()
            );
        });
        shot.act(|w, window, cx| {
            let editor = w.active_tab().editor.clone();
            editor.update(cx, |editor, cx| {
                editor.set_value("select * from orders", window, cx)
            });
        });

        shot.act(|w, window, cx| w.open_settings(&crate::OpenSettings, window, cx));
        shot.save(&format!("15-settings-{suffix}"));
        shot.act(|w, _, cx| {
            w.settings = None;
            cx.notify();
        });
        // Another accent: everything that was silver turns amber, numbers turn lilac.
        shot.act(|w, window, cx| {
            let prefs = crate::settings::Prefs {
                accent: crate::theme::Accent::Amber,
                ..w.prefs
            };
            w.set_prefs(prefs, window, cx)
        });
        shot.save(&format!("16-amber-{suffix}"));
        shot.act(|w, window, cx| {
            let prefs = crate::settings::Prefs {
                accent: crate::theme::Accent::Xenon,
                ..w.prefs
            };
            w.set_prefs(prefs, window, cx)
        });

        shot.act(|w, window, cx| w.open_palette(&crate::OpenPalette, window, cx));
        shot.save(&format!("04-palette-{suffix}"));
        shot.act(|w, _, cx| {
            w.palette = None;
            cx.notify();
        });

        shot.act(|w, window, cx| {
            let editor = w.active_tab().editor.clone();
            editor.update(cx, |e, cx| {
                e.set_value("select c.name, count(*) as orders, sum(o.total) as revenue\nfrom orders o join customers c on c.id = o.customer_id\ngroup by c.name\norder by revenue desc;", window, cx)
            });
            w.run(RunScope::All, window, cx);
        });
        shot.act(|w, window, cx| w.show_history(&crate::ShowHistory, window, cx));
        shot.save(&format!("05-query-history-{suffix}"));

        shot.act(|w, window, cx| {
            if let Some(session) = w.sessions.first_mut() {
                session.environment = Environment::Production;
            }
            let editor = w.active_tab().editor.clone();
            editor.update(cx, |e, cx| e.set_value("delete from orders", window, cx));
            w.run(RunScope::All, window, cx);
        });
        shot.save(&format!("06-confirm-{suffix}"));

        // Two connections open side by side.
        let mut shot = Shot::new(mode, Some(&shop), &root);
        shot.act(|w, window, cx| w.open_path(second.clone(), window, cx));
        shot.act(|w, window, cx| {
            let id = w.sessions[1].id;
            w.browse(id, "main", "events", window, cx)
        });
        shot.save(&format!("07-two-connections-{suffix}"));
        shot.act(|w, window, cx| w.open_palette(&crate::OpenPalette, window, cx));
        shot.save(&format!("08-palette-two-{suffix}"));
        shot.act(|w, _, cx| {
            w.palette = None;
            cx.notify();
        });
        shot.act(|w, window, cx| w.open_connect_tab(window, cx));
        shot.save(&format!("09-connect-tab-{suffix}"));

        shot.act(|w, window, cx| {
            w.new_server_connection(xenon_store::ServerEngine::Mysql, window, cx)
        });
        shot.save(&format!("10-connect-mysql-{suffix}"));
        shot.act(|w, _, cx| {
            if let Some((dialog, _)) = &w.connect_dialog {
                dialog.update(cx, |dialog, cx| dialog.show_ssh(cx));
            }
        });
        shot.save(&format!("12-connect-ssh-{suffix}"));
        shot.act(|w, _, cx| {
            w.connect_dialog = None;
            cx.notify();
        });

        // A live MariaDB from the test fleet (dev/fleet/up.sh mariadb), when asked for.
        if std::env::var_os("XENON_SNAPSHOT_LIVE").is_some() {
            shot.act(|w, window, cx| {
                w.connect_url(
                    xenon_store::ServerEngine::Mysql,
                    "mysql://xenon:xenon@127.0.0.1:53307/shop".into(),
                    window,
                    cx,
                )
            });
            shot.act(|w, window, cx| {
                let id = w.sessions.last().expect("connected to the fleet").id;
                w.browse(id, "shop", "type_zoo", window, cx)
            });
            shot.save(&format!("11-mariadb-type-zoo-{suffix}"));

            // And Postgres through the fleet's SSH bastion.
            shot.act(|w, window, cx| {
                w.connect_server(
                    xenon_store::ServerEngine::Postgres,
                    xenon_driver::ConnectOptions {
                        host: "postgres".into(),
                        port: Some(5432),
                        database: "shop".into(),
                        user: "xenon".into(),
                        password: "xenon".into(),
                        ssh: Some(xenon_driver::SshOptions {
                            host: "127.0.0.1".into(),
                            port: Some(52222),
                            user: "xenon".into(),
                            password: "xenon".into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            shot.act(|w, window, cx| {
                let id = w.sessions.last().expect("tunnelled to the fleet").id;
                w.browse(id, "public", "orders", window, cx)
            });
            shot.save(&format!("13-postgres-over-ssh-{suffix}"));

            // SQL Server 2022 from the fleet (dev/fleet/up.sh mssql).
            shot.act(|w, window, cx| {
                w.connect_server(
                    xenon_store::ServerEngine::SqlServer,
                    xenon_driver::ConnectOptions {
                        host: "127.0.0.1".into(),
                        port: Some(51433),
                        database: "shop".into(),
                        user: "sa".into(),
                        password: "Xenon-fleet-1".into(),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            shot.act(|w, window, cx| {
                let id = w.sessions.last().expect("connected to SQL Server").id;
                w.browse(id, "dbo", "type_zoo", window, cx)
            });
            shot.save(&format!("14-sqlserver-type-zoo-{suffix}"));
        }

        // Closing a connection closes only its tabs; the other stays open.
        shot.act(|w, window, cx| {
            let (first, open) = (w.sessions[0].id, w.sessions.len());
            w.close_session(first, window, cx);
            assert_eq!(w.sessions.len(), open - 1);
            assert!(w.tabs().iter().all(|tab| tab.session != Some(first)));
        });
    }
}
