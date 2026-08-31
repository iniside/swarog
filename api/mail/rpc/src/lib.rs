//! `mailrpc` — mail's minimal GENERATED-glue crate. mail has no sync capability of its
//! own (no `api/mail/api` — outbound send is durable-only), so this crate carries TWO
//! re-exports: the cross-cutting admin fan-out's server-side [`register_admin`] and
//! [`register_admin_submit`] (the Mail page is remotely editable — parked-row requeue/
//! cancel and the send-test form). The `mail` MODULE registers its
//! `admin.adminData`/`admin.adminSubmit` edge handlers through THIS crate (its OWN
//! `<name>rpc`) instead of importing `adminrpc` directly — archcheck forbids a module →
//! foreign-rpc edge, but the module → own-rpc → adminrpc chain is sanctioned (rule 5),
//! exactly the characters/inventory pattern.

/// The admin fan-out's server-side registration (`register_admin(server, svc)`),
/// re-exported from the cross-cutting `adminrpc` glue so the `mail` module reaches it
/// through its own glue crate — never importing `adminrpc` itself.
pub use adminrpc::register_admin;

/// The admin fan-out's server-side WRITE registration
/// (`register_admin_submit(server, svc)`), re-exported the same way so the `mail`
/// module's admin page stays editable from a remote admin process.
pub use adminrpc::register_admin_submit;
