//! The internal typed model — the contract the Step-3 C# emitter will consume.
//!
//! Everything here is serde-serializable with a STABLE field order (structs serialize
//! in declaration order; the vectors are sorted deterministically by the scraper), so
//! `--emit-manifest` produces byte-stable JSON suitable for a committed golden and a
//! future `git diff --exit-code` freshness gate. Kept deliberately minimal: 6 DTOs and
//! 12 methods do not justify a richer type lattice.

use serde::{Deserialize, Serialize};

/// A reference to a type at a method/DTO seam, reduced to the small set the player
/// surface actually uses. `Struct(name)` names a DTO in [`Manifest::dtos`]; everything
/// else is a scalar/shape the emitter maps to a C# primitive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypeRef {
    /// A Rust `String` → C# `string`.
    String,
    /// A Rust `i64` → C# `long`.
    I64,
    /// A `Result<(), _>` return: no value rides the wire.
    Unit,
    /// A `Vec<T>` → C# `T[]`.
    Vec(Box<TypeRef>),
    /// A named DTO (present in [`Manifest::dtos`]) → the C# record of that name.
    Struct(String),
}

/// One method argument (after the leading `Identity` strip). `wire_name` is the JSON
/// key the value travels under in the wire request struct — the `body_names` override
/// when one exists, else the param name (path-wildcard args keep the param name, since
/// the QUIC player plane sends the wire request struct directly, with no HTTP decode).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgDef {
    pub name: String,
    pub wire_name: String,
    pub ty: TypeRef,
}

/// One player-reachable method: transport facts (from runtime `route_bindings()`) plus
/// typed shape (from the `syn` source parse), cross-checked by the drift gate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MethodDef {
    pub provider: String,
    pub wire_method: String,
    pub verb: String,
    pub path: String,
    /// `"none"` or `"player"` (the `AuthReq` variant, lowercased).
    pub auth: String,
    pub success: u16,
    /// The wire arguments in declaration order. Empty for a no-arg method — the Step-3
    /// emitter still emits a request DTO and always serializes `{}` (never `null`), the
    /// live-verified requirement from Step 1.
    pub args: Vec<ArgDef>,
    pub ret: TypeRef,
}

/// One DTO field. `wire_name` is the serde key (the `#[serde(rename = "...")]` override
/// when present, else the field name — these DTOs are already snake_case by default).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDef {
    pub name: String,
    pub wire_name: String,
    pub ty: TypeRef,
}

/// A DTO reachable from a method arg or return type (recursively).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DtoDef {
    pub name: String,
    pub fields: Vec<FieldDef>,
}

/// The whole scraped contract: methods, the DTOs they reference, and the `Status`
/// enum's variant names (the domain-outcome taxonomy the client throws on).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub methods: Vec<MethodDef>,
    pub dtos: Vec<DtoDef>,
    pub statuses: Vec<String>,
}
