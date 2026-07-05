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
 * Channel/gRPC-server/endpoint gating is a config-profile concern, not this bean's.
 */
@ApplicationScoped
class RoleConfig(
    @ConfigProperty(name = "roles", defaultValue = "all") private val roles: Set<String>,
) {
    fun isActive(module: String): Boolean = "all" in roles || module in roles
}
