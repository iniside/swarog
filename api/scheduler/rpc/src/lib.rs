//! `schedulerrpc` — scheduler's minimal GENERATED-glue crate. The scheduler is a PURE
//! durable event SOURCE with no sync capability of its own (no `api/scheduler/api`,
//! mirroring Go's `modules/scheduler`), so this crate carries a SINGLE re-export: the
//! cross-cutting admin fan-out's server-side [`register_admin`]. The `scheduler` MODULE
//! registers its read-only `admin.adminData` "Schedules" face through THIS crate (its
//! OWN `<name>rpc`) instead of importing `adminrpc` directly — archcheck forbids a
//! module → foreign-rpc edge, but the module → own-rpc → adminrpc chain is sanctioned
//! (rule 5), exactly the audit pattern.

/// The admin fan-out's server-side registration (`register_admin(server, svc)`),
/// re-exported from the cross-cutting `adminrpc` glue so the `scheduler` module reaches
/// it through its own glue crate — never importing `adminrpc` itself.
pub use adminrpc::register_admin;
