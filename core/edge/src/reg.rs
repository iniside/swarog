//! [`EdgeReg`] — a module's topology-blind internal-edge registration, plus the
//! [`EDGE_SLOT`] it is contributed to.
//!
//! The seam that keeps domain modules ignorant of the process topology: a module
//! contributes an `EdgeReg` UNCONDITIONALLY during `init` (wrapping its own
//! generated `register_server` glue), and `app::run` applies the contributions iff
//! the entrypoint passed an internal edge [`Server`]. In the monolith they are
//! simply never applied — the module holds no `Option`, takes no branch, and never
//! learns whether this process serves a QUIC edge.
//!
//! Lives HERE (not `opsapi`): the closure names `&mut Server`, and `edge` already
//! depends on `opsapi`, so hosting the type there would be a dependency cycle.

use std::sync::{Arc, Mutex};

use crate::Server;

/// The contrib slot edge registrations are contributed to (`ctx.contribute`) and
/// `app::run` drains (`ctx.contributions::<EdgeReg>`).
pub const EDGE_SLOT: contrib::Slot<EdgeReg> = contrib::Slot::new("edge.registration");

/// The registration closure: everything a module wants installed on the process's
/// shared internal edge server (typically one `register_server` call per generated
/// RPC face).
type RegisterFn = Box<dyn FnOnce(&mut Server) + Send>;

/// One module's edge registration, waiting in the [`EDGE_SLOT`] until `app::run`
/// decides whether this process has an edge server to apply it to.
///
/// Mechanics: the contrib registry hands contributions back BY CLONE
/// (`Slots::contributions` requires `Clone`), but registration is a one-shot
/// `FnOnce` (it moves the served `Arc<Service>` in). So the closure sits behind an
/// `Arc<Mutex<Option<…>>>`: clones share the ONE closure, and [`EdgeReg::apply`]
/// `take`s it — applied at most once by construction, no matter how many clones the
/// slot handed out.
#[derive(Clone)]
pub struct EdgeReg(Arc<Mutex<Option<RegisterFn>>>);

impl EdgeReg {
    /// Wraps a module's registration closure. The closure receives the process's
    /// shared internal edge [`Server`] and installs the module's handlers on it.
    pub fn new(f: impl FnOnce(&mut Server) + Send + 'static) -> EdgeReg {
        EdgeReg(Arc::new(Mutex::new(Some(Box::new(f)))))
    }

    /// Runs the registration against `server`. At most once across ALL clones of
    /// this registration: a second call (or a call on another clone) is a no-op.
    pub fn apply(&self, server: &mut Server) {
        if let Some(f) = self.0.lock().unwrap().take() {
            f(server);
        }
    }
}

#[cfg(test)]
#[path = "reg_tests.rs"]
mod tests;
