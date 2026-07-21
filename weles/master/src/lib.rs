//! `weles-master` — the PLATFORM-FREE half of weles: the future-master role,
//! drawn as a real crate boundary so the compiler (not a comment, not a grep)
//! keeps master code off the agent's platform I/O.
//!
//! # Role
//!
//! Master owns the fleet as DATA and the pure answers derived from it:
//!
//! * [`manifest`] — the runtime fleet types ([`manifest::ServiceDef`],
//!   [`manifest::Addrs`], [`manifest::AddrKind`]), the one address formatter,
//!   the peer-address derivation ([`manifest::PeerAddrs`] — what a `resolve`
//!   answers from), and the spawn-env composer.
//! * [`fleet_toml`] — the strict operator-authored `fleet.toml` loader +
//!   topology-generic validator, and the parsed `[[prepare]]` DATA
//!   ([`fleet_toml::PrepareCmd`]) the agent later executes.
//! * [`state`] — the persisted supervisor-state schema
//!   ([`state::FleetState`] and friends) + its atomic checkpoint/load.
//!
//! # The compiler-enforced boundary
//!
//! The agent half (the `weles` crate) owns ALL platform I/O:
//! `platform`/`supervisor`/`lock`/`prep`/`control`/`agentapi`. The dependency
//! points agent -> master ONLY: `weles` depends on `weles-master`, never the
//! reverse (a cycle is forbidden). So master code physically cannot name the
//! agent's platform modules — there is no crate in `weles-master`'s dependency
//! graph that provides them, so the `use` does not resolve:
//!
//! ```compile_fail
//! // The `weles` (agent) crate is NOT — and never can be — a dependency of
//! // `weles-master`. Every one of these fails to compile with E0432
//! // "unresolved import", which is exactly the master/agent role boundary
//! // enforced by the compiler rather than by a source-string grep or a comment.
//! use weles::platform;
//! use weles::supervisor;
//! use weles::lock;
//! ```
//!
//! And the platform modules are simply absent from this crate — there is no
//! `weles_master::platform` to reach for either:
//!
//! ```compile_fail
//! // No such module in `weles-master`: platform I/O lives on the agent side.
//! let _ = weles_master::platform::spawn;
//! ```
//!
//! This models the future single-binary -> two-process split (master and agent
//! as separate processes over an RPC hop) without splitting the process yet:
//! today `weles` links `weles-master` in-process and hands values across the
//! seam directly.

pub mod fleet_toml;
pub mod manifest;
pub mod state;
