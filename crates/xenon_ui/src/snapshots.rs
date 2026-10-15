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
        let mut shot = Shot::new(mode, None, &root);
        shot.save(&format!("01-connect-{suffix}"));
        shot.act(|w, window, cx| w.new_connection(&crate::NewConnection, window, cx));
        shot.save(&format!("02-connect-dialog-{suffix}"));

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
