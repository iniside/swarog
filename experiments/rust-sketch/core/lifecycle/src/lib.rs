//! Wires modules together: the [`Context`] handed to each module at startup and
//! the [`App`] that builds/migrates/starts/stops them. It imports the three leaf
//! foundations (`bus`, `registry`, `contrib`) plus stdlib; nothing in those leaves
//! imports `lifecycle`, so the import graph stays acyclic.

mod app;
mod context;
mod module;

pub use app::App;
pub use context::Context;
pub use module::{Caps, Module};

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Records every lifecycle callback into a shared log so a test can assert
    /// phase ordering. Opts into all four optional phases so all fire.
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

        fn caps(&self) -> Caps {
            Caps::REGISTER | Caps::MIGRATE | Caps::START | Caps::STOP
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

    /// A module that opts into NO optional phases is only ever `init`-ed — its
    /// (default no-op) migrate/start/stop are never invoked.
    #[tokio::test]
    async fn plain_module_only_inits() {
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
}
