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

use gpui::{
    AnyWindowHandle, AppContext as _, Entity, HeadlessAppContext, Window, px, size,
};
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
        Store::open(&data).unwrap().set_setting("theme", value).unwrap();
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
        Self { cx, window: window.into(), workspace, out: out.to_path_buf() }
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
    let names = ["Aoife Byrne", "Liam Walsh", "Priya Nair", "Tom Okafor", "Mei Chen", "Ana Silva", "Jonas Berg"];
    let plans = ["free", "pro", "team", "pro", "free"];
    for i in 1..=240 {
        let name = names[i % names.len()];
        db.execute(
            "insert into customers values (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                i as i64,
                format!("{}.{}@example.com", name.split(' ').next().unwrap().to_lowercase(), i),
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
                if i % 17 == 0 { Some("{\"gift\": true, \"wrap\": \"blue\"}") } else { None },
                format!("2026-{:02}-{:02} {:02}:{:02}:00", 1 + i % 10, 1 + i % 27, 8 + i % 10, i % 60)
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
        target: Target::Postgres {
            host: host.into(),
            port: Some(5432),
            database: db.into(),
            user: "app".into(),
            ssl: "require".into(),
        },
        environment,
        read_only: false,
    };
    store.save_connection(&pg("Billing", "db.billing.internal", "billing", Environment::Production)).unwrap();
    store
        .save_connection(&SavedConnection {
            read_only: true,
            ..pg("Analytics replica", "replica.analytics.internal", "warehouse", Environment::Production)
        })
        .unwrap();
    store.save_connection(&pg("Staging", "staging.internal", "app", Environment::Development)).unwrap();
    store
        .save_connection(&SavedConnection {
            id: 0,
            name: "shop.db".into(),
            target: Target::Sqlite { path: shop.display().to_string() },
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
    seed_saved(&Store::open(&data.join("xenon.db")).unwrap(), &shop);

    for (mode, suffix) in [(Mode::Dark, "dark"), (Mode::Light, "light")] {
        let mut shot = Shot::new(mode, None, &root);
        shot.save(&format!("01-connect-{suffix}"));
        shot.act(|w, window, cx| w.new_connection(&crate::NewConnection, window, cx));
        shot.save(&format!("02-connect-dialog-{suffix}"));

        let mut shot = Shot::new(mode, Some(&shop), &root);
        shot.act(|w, window, cx| w.browse("main", "orders", window, cx));
        shot.act(|w, window, cx| {
            w.select_cell(2, 2, false, window, cx);
            w.select_cell(5, 4, true, window, cx);
        });
        shot.save(&format!("03-browse-{suffix}"));

        shot.act(|w, window, cx| w.open_palette(&crate::OpenPalette, window, cx));
        shot.save(&format!("04-palette-{suffix}"));
        shot.act(|w, _, cx| { w.palette = None; cx.notify(); });

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
            if let Some(session) = &mut w.session {
                session.environment = Environment::Production;
            }
            let editor = w.active_tab().editor.clone();
            editor.update(cx, |e, cx| e.set_value("delete from orders", window, cx));
            w.run(RunScope::All, window, cx);
        });
        shot.save(&format!("06-confirm-{suffix}"));
    }
}
