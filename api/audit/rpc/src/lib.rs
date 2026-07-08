//! `auditrpc` — audit's minimal GENERATED-glue crate. audit is a PURE SINK with no
//! sync capability of its own (no `api/audit/api`, mirroring Go's `modules/audit`), so
//! this crate carries a SINGLE re-export: the cross-cutting admin fan-out's
//! server-side [`register_admin`]. The `audit` MODULE registers its `admin.adminData`
//! edge handler through THIS crate (its OWN `<name>rpc`) instead of importing
//! `adminrpc` directly — archcheck forbids a module → foreign-rpc edge, but the
//! module → own-rpc → adminrpc chain is sanctioned (rule 5), exactly the
//! characters/inventory pattern.

/// The admin fan-out's server-side registration (`register_admin(server, svc)`),
/// re-exported from the cross-cutting `adminrpc` glue so the `audit` module reaches it
/// through its own glue crate — never importing `adminrpc` itself.
pub use adminrpc::register_admin;
