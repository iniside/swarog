package platform

import jakarta.enterprise.context.ApplicationScoped
import org.eclipse.microprofile.config.inject.ConfigProperty

/**
 * Which module "roles" this process runs. One artifact, deployed 1× (monolith) or N× (a subset of
 * roles per process) — the difference is only this config. `ROLES=accounts,characters` (env, comma
 * separated) is auto-converted by SmallRye to `Set<String>`; unset ⇒ default `all` ⇒ every module
 * active, exactly as before.
 *
 * Gates ONLY module startup/migrations (and, later, the produced-bean local/remote branch).
 * Channel/edge-server/endpoint gating is a config-profile concern, not this bean's.
 */
@ApplicationScoped
class RoleConfig(
    @ConfigProperty(name = "roles", defaultValue = "all") roles: Set<String>,
) {
    // Case-insensitive: `ROLES=ALL`, `All`, and `all` must all mean the monolith, and `Inventory`
    // must gate the same module as `inventory`. Normalizing the configured set (and lowercasing the
    // queried module below) removes the silent ops footgun where an upper/mixed-case value activated
    // NOTHING because membership was compared case-sensitively.
    private val normalized: Set<String> = roles.mapTo(HashSet(roles.size)) { it.lowercase() }

    fun isActive(module: String): Boolean = "all" in normalized || module.lowercase() in normalized

    /** True for the monolith (roles=all, everything co-located) — no split process. */
    fun isMonolith(): Boolean = "all" in normalized
}
