//! `mailrpc` — mail's minimal GENERATED-glue crate. mail has no sync capability of its
//! own (no `api/mail/api` — outbound send is durable-only), so this crate re-exports the
//! cross-cutting admin fan-out's server-side [`register_admin`] and
//! [`register_admin_submit`] (the Mail page is remotely editable). The `mail` MODULE
//! registers its `admin.adminData`/`admin.adminSubmit` edge handlers through THIS crate
//! rather than importing `adminrpc` directly — archcheck forbids a module → foreign-rpc
//! edge, but the module → own-rpc → adminrpc chain is sanctioned (rule 3), exactly the
//! characters/inventory pattern.

pub use adminrpc::{register_admin, register_admin_submit};
