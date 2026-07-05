package platform

/**
 * The `platform` module — cross-cutting infrastructure shared by impl modules, deliberately
 * free of any feature knowledge (no accounts/characters/inventory/admin types).
 *
 * Empty placeholder for now: later steps of the dual-deploy plan home `RoleConfig` (startup
 * gating) and the transactional-outbox row-model + mark-sent SQL helper here. Kept as a real
 * Gradle module so those additions land without touching the module graph again.
 */
internal object Platform
