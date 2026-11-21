//! The portal's template layer: the view models the templates render, their conversion from
//! control-plane types, and the embedded templates.

use minijinja::{Environment, Value};
use serde::Serialize;

/// The enum variants the render path converts into view models; recorded only in test builds.
#[cfg(test)]
pub(crate) mod seen {
    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet};

    use controlplane::Variants;

    /// One converted enum: every variant its type declares and the ones converted.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) struct Converted {
        pub(crate) declared: &'static [&'static str],
        pub(crate) converted: BTreeSet<&'static str>,
    }

    /// Each converted enum by name.
    pub(crate) type Record = BTreeMap<&'static str, Converted>;

    thread_local! {
        // Per thread, since cargo runs tests on parallel threads: a collect sees only its own thread's renders.
        static RECORD: RefCell<Option<Record>> = const { RefCell::new(None) };
    }

    pub(crate) fn record<E: Variants>(value: &E) {
        RECORD.with_borrow_mut(|record| {
            if let Some(record) = record {
                record
                    .entry(E::NAME)
                    .or_insert_with(|| Converted { declared: E::VARIANTS, converted: BTreeSet::new() })
                    .converted
                    .insert(value.variant());
            }
        });
    }

    struct Collecting;

    impl Drop for Collecting {
        fn drop(&mut self) {
            RECORD.set(None);
        }
    }

    /// Runs `f` and returns what the render path converted on this thread while it ran;
    /// nothing converted before or after, or on another thread, counts.
    pub(crate) fn collect<R>(f: impl FnOnce() -> R) -> (R, Record) {
        assert!(RECORD.with_borrow(Option::is_none), "seen::collect does not nest");
        RECORD.set(Some(Record::new()));
        let _collecting = Collecting;
        let out = f();
        (out, RECORD.take().unwrap_or_default())
    }

    #[test]
    fn collect_holds_only_its_own_threads_conversions_while_it_runs() {
        use controlplane::{Emphasis, Point, Tone};

        super::mode_of(&Point::NavCounter, false);
        let ((), record) = collect(|| {
            std::thread::spawn(|| super::emphasis_name(Emphasis::Ghost)).join().expect("thread");
            super::mode_of(&Point::DashboardTile, false);
            super::tone(Tone::Warn);
            crate::role_name(controlplane::Role::Viewer);
        });
        super::tone(Tone::Err);
        assert_eq!(record.keys().copied().collect::<Vec<_>>(), ["Point", "Role", "Tone"]);
        assert_eq!(record["Role"].converted, BTreeSet::from(["Viewer"]));
        assert_eq!(record["Point"].declared, Point::VARIANTS);
        assert_eq!(record["Point"].converted, BTreeSet::from(["DashboardTile"]));
        assert_eq!(record["Tone"].converted, BTreeSet::from(["Warn"]));
        assert!(collect(|| ()).1.is_empty());
    }
}

#[cfg(not(test))]
pub(crate) mod seen {
    #[inline(always)]
    pub(crate) fn record<E: controlplane::Variants>(_: &E) {}
}

/// The refs the draw looks a label up for; recorded only in test builds.
#[cfg(test)]
pub(crate) mod asked {
    use std::cell::RefCell;
    use std::collections::BTreeSet;

    use controlplane::EntityRef;

    /// Each looked-up ref as `(kind, id)`.
    pub(crate) type Asked = BTreeSet<(String, String)>;

    thread_local! {
        static ASKED: RefCell<Option<Asked>> = const { RefCell::new(None) };
    }

    pub(crate) fn record(e: &EntityRef) {
        ASKED.with_borrow_mut(|asked| {
            if let Some(asked) = asked {
                asked.insert((e.kind.clone(), e.id.clone()));
            }
        });
    }

    struct Collecting;

    impl Drop for Collecting {
        fn drop(&mut self) {
            ASKED.set(None);
        }
    }

    /// Runs `f` and returns the refs the draw looked up on this thread while it ran.
    pub(crate) fn collect<R>(f: impl FnOnce() -> R) -> (R, Asked) {
        assert!(ASKED.with_borrow(Option::is_none), "asked::collect does not nest");
        ASKED.set(Some(Asked::new()));
        let _collecting = Collecting;
        let out = f();
        (out, ASKED.take().unwrap_or_default())
    }
}

#[cfg(not(test))]
pub(crate) mod asked {
    #[inline(always)]
    pub(crate) fn record(_: &controlplane::EntityRef) {}
}

/// The hints of the actions `routes::action_item` refuses, each the title of the disabled
/// control drawn for it; recorded only in test builds.
#[cfg(test)]
pub(crate) mod refused {
    use std::cell::RefCell;
    use std::collections::BTreeSet;

    /// Each refused action's hint.
    pub(crate) type Refused = BTreeSet<String>;

    thread_local! {
        static REFUSED: RefCell<Option<Refused>> = const { RefCell::new(None) };
    }

    pub(crate) fn record(hint: &str) {
        REFUSED.with_borrow_mut(|refused| {
            if let Some(refused) = refused {
                refused.insert(hint.to_string());
            }
        });
    }

    struct Collecting;

    impl Drop for Collecting {
        fn drop(&mut self) {
            REFUSED.set(None);
        }
    }

    /// Runs `f` and returns the hints of the actions refused on this thread while it ran.
    pub(crate) fn collect<R>(f: impl FnOnce() -> R) -> (R, Refused) {
        assert!(REFUSED.with_borrow(Option::is_none), "refused::collect does not nest");
        REFUSED.set(Some(Refused::new()));
        let _collecting = Collecting;
        let out = f();
        (out, REFUSED.take().unwrap_or_default())
    }
}

#[cfg(not(test))]
pub(crate) mod refused {
    #[inline(always)]
    pub(crate) fn record(_: &str) {}
}

mod url;
mod style;
mod cell;
mod action;
mod shell;
mod panel;
mod table;
mod tiles;
mod form;
mod queue;
mod modal;

pub(crate) use self::url::*;
pub(crate) use self::style::*;
pub(crate) use self::cell::*;
pub(crate) use self::action::*;
pub(crate) use self::shell::*;
pub(crate) use self::panel::*;
pub(crate) use self::table::*;
pub(crate) use self::tiles::*;
pub(crate) use self::form::*;
pub(crate) use self::queue::*;
pub(crate) use self::modal::*;

const SHELL: &str = include_str!("../templates/shell.html.tmpl");
const PRIMITIVES: &str = include_str!("../templates/primitives.html.tmpl");
const MODAL: &str = include_str!("../templates/modal.html.tmpl");
const LOGIN: &str = include_str!("../templates/login.html.tmpl");

/// Compiles the embedded templates; `.html` names turn auto-escaping on.
pub(crate) fn env() -> anyhow::Result<Environment<'static>> {
    let mut env = Environment::new();
    for (name, source) in [
        ("primitives.html", PRIMITIVES),
        ("shell.html", SHELL),
        ("modal.html", MODAL),
        ("login.html", LOGIN),
    ] {
        env.add_template(name, source)
            .map_err(|e| anyhow::anyhow!("admin: template {name} compile: {e}"))?;
    }
    Ok(env)
}

pub(crate) fn page<S: Serialize>(env: &Environment<'_>, template: &str, view: &S) -> Result<String, minijinja::Error> {
    env.get_template(template)?.render(view)
}

/// A full screen: the shell with `screen` naming the screen and `v` its view.
pub(crate) fn screen_page<S: Serialize>(env: &Environment<'_>, screen: &str, view: &S) -> Result<String, minijinja::Error> {
    page(env, "shell.html", &minijinja::context! { screen => screen, v => Value::from_serialize(view) })
}

pub(crate) fn fragment<S: Serialize>(
    env: &Environment<'_>,
    template: &str,
    name: &str,
    arg: &S,
) -> Result<String, minijinja::Error> {
    let captured = env.get_template(template)?.render_captured(minijinja::context! {})?;
    captured.state().call_macro(name, &[Value::from_serialize(arg)])
}
