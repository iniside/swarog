// ============================================================================
// Owner — the polymorphic owner. Referenced by id, no cross-module FK.
// ============================================================================

/// Who an inventory belongs to. `otype` is `"player"` or `"character"`; `id` is the
/// player/character uuid. The polymorphism lives entirely inside this module.
pub(crate) struct Owner {
    pub(crate) otype: String,
    pub(crate) id: String,
}

impl Owner {
    pub(crate) fn player(id: impl Into<String>) -> Owner {
        Owner { otype: "player".into(), id: id.into() }
    }
    pub(crate) fn character(id: impl Into<String>) -> Owner {
        Owner { otype: "character".into(), id: id.into() }
    }
    pub(crate) fn new(otype: &str, id: &str) -> Owner {
        Owner { otype: otype.into(), id: id.into() }
    }
}
