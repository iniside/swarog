//! Wires modules together: the [`Context`] handed to each module at startup and
//! the [`App`] that builds/migrates/starts/stops them. It imports the three leaf
//! foundations (`bus`, `registry`, `contrib`) plus stdlib; nothing in those leaves
//! imports `lifecycle`, so the import graph stays acyclic.

mod app;
mod context;
mod ddl;
mod module;
mod wiring;

pub use app::App;
pub use context::Context;
pub use ddl::Ddl;
pub use module::Module;
pub use wiring::ProcessWiring;

#[cfg(test)]
mod tests;
