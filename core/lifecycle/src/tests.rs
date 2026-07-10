use std::sync::{Arc, Mutex};

use super::*;

/// Records every lifecycle callback into a shared log so a test can assert
/// phase ordering.
struct RecMod {
    name: String,
    log: Arc<Mutex<Vec<String>>>,
}

impl RecMod {
    fn boxed(name: &str, log: &Arc<Mutex<Vec<String>>>) -> Box<dyn Module> {
        Box::new(RecMod {
            name: name.to_string(),
            log: log.clone(),
        })
    }

    fn record(&self, phase: &str) {
        self.log.lock().unwrap().push(format!("{phase}:{}", self.name));
    }
}

#[async_trait::async_trait]
impl Module for RecMod {
    fn name(&self) -> &str {
        &self.name
    }

    fn register(&self, _ctx: &Context) -> anyhow::Result<()> {
        self.record("register");
        Ok(())
    }

    fn init(&self, _ctx: &Context) -> anyhow::Result<()> {
        self.record("init");
        Ok(())
    }

    async fn migrate(&self, _ctx: &Context) -> anyhow::Result<()> {
        self.record("migrate");
        Ok(())
    }

    async fn start(&self, _ctx: &Context) -> anyhow::Result<()> {
        self.record("start");
        Ok(())
    }

    async fn stop(&self, _ctx: &Context) -> anyhow::Result<()> {
        self.record("stop");
        Ok(())
    }
}

fn app_with(log: &Arc<Mutex<Vec<String>>>, names: &[&str]) -> App {
    let mut app = App::new(Arc::new(Context::new()));
    for n in names {
        app.add(RecMod::boxed(n, log));
    }
    app
}

/// The core guarantee: ALL registers run before ANY init (phase 1 → phase 2),
/// each phase in registration order. That's what lets a module require any
/// service in init without a topological sort.
#[tokio::test]
async fn two_phase_build() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let app = app_with(&log, &["a", "b"]);
    app.build().unwrap();
    assert_eq!(
        *log.lock().unwrap(),
        vec!["register:a", "register:b", "init:a", "init:b"]
    );
}

/// Migrate and start run in registration order after build; stop runs in
/// REVERSE registration order.
#[tokio::test]
async fn full_lifecycle_ordering() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let app = app_with(&log, &["a", "b"]);
    app.build().unwrap();
    app.migrate().await.unwrap();
    app.start().await.unwrap();
    app.stop().await;
    assert_eq!(
        *log.lock().unwrap(),
        vec![
            "register:a",
            "register:b",
            "init:a",
            "init:b",
            "migrate:a",
            "migrate:b",
            "start:a",
            "start:b",
            "stop:b",
            "stop:a",
        ]
    );
}

#[tokio::test]
#[should_panic(expected = "registered twice")]
async fn duplicate_name_panics() {
    let log = Arc::new(Mutex::new(Vec::new()));
    app_with(&log, &["dup", "dup"]);
}

/// A module implementing only `name`/`init` (all other phases left at their
/// default no-op impls) still gets every phase called unconditionally — the
/// full build/migrate/start/stop cycle must succeed on a DB-less `Context`
/// without error, proving the defaults are true no-ops.
#[tokio::test]
async fn default_impl_module_survives_full_cycle() {
    struct Plain {
        log: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl Module for Plain {
        fn name(&self) -> &str {
            "plain"
        }
        fn init(&self, _ctx: &Context) -> anyhow::Result<()> {
            self.log.lock().unwrap().push("init:plain".into());
            Ok(())
        }
    }

    let log = Arc::new(Mutex::new(Vec::new()));
    let mut app = App::new(Arc::new(Context::new()));
    app.add(Box::new(Plain { log: log.clone() }));
    app.build().unwrap();
    app.migrate().await.unwrap();
    app.start().await.unwrap();
    app.stop().await;
    assert_eq!(*log.lock().unwrap(), vec!["init:plain"]);
}
