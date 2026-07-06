package gateway

import io.quarkus.vertx.web.Route
import io.vertx.core.Vertx
import io.vertx.ext.web.RoutingContext
import io.vertx.httpproxy.HttpProxy
import jakarta.enterprise.context.ApplicationScoped
import org.eclipse.microprofile.config.inject.ConfigProperty
import platform.RoleConfig

/**
 * The HTTP side of the front door: the `admin`, `characters`, and `inventory` path prefixes are
 * byte-for-byte reverse-proxied (Vert.x [HttpProxy] — hop-by-hop headers, streaming, and WebSocket
 * upgrades handled for free) to the owning service's plain `host:port`, mirroring the QUIC side's
 * routing table ([GatewayEdgeServer]'s `gateway.route` targets) rather than Stork.
 *
 * Origins come from PLAIN config, not Stork: `gateway.http.admin.target`, `.characters.target`,
 * `.inventory.target`, each defaulting to the SAME `_ADDR` envs the split already sets
 * (`ADMIN_ADDR`, `CHARACTERS_ADDR`, `INVENTORY_ADDR`). The admin console is hosted IN
 * `inventory-service` today (see `inventory-service/application.properties` — `admin` rides its role
 * list alongside `inventory`), so `gateway.http.admin.target` defaults to the same address as
 * `gateway.http.inventory.target` (`localhost:8081`) — verified against the actual topology, not
 * assumed.
 *
 * Path preservation (why this doesn't proxy into a 404): [HttpProxy.handle] forwards the FULL inbound
 * request (method, path, headers, body) to the origin unchanged — a request under the `admin` prefix
 * arrives at the origin with the identical path. Checked against the real backends:
 * `admin.AdminResource` is rooted at `admin` (with a `{slug}` sub-path), `characters.CharactersResource`
 * is rooted at `characters` (with an `{id}` sub-path), `inventory.InventoryResource` is rooted at
 * `inventory` (with a `{characterId}/grant` sub-path) — all three prefixes land on a real resource,
 * verbatim.
 *
 * Role-gated like [GatewayEdgeServer]: this bean only ever ships on `gateway-service`'s classpath (no
 * other app-shell depends on the `gateway` module), but each handler still declines with `ctx.next()`
 * (falling through to a plain 404, since nothing else registers these paths) unless
 * `roleConfig.isActive("gateway")` — the same defense-in-depth the QUIC side applies, in case `gateway`
 * ever rode a shared classpath.
 */
@ApplicationScoped
class GatewayHttpProxy(
    vertx: Vertx,
    private val roleConfig: RoleConfig,
    @ConfigProperty(name = "gateway.http.admin.target") adminTarget: String,
    @ConfigProperty(name = "gateway.http.characters.target") charactersTarget: String,
    @ConfigProperty(name = "gateway.http.inventory.target") inventoryTarget: String,
) {
    // One shared HttpClient underlies all three origins; each HttpProxy just pins a different fixed
    // origin on top of it.
    private val httpClient = vertx.createHttpClient()

    private val adminProxy = buildProxy(httpClient, adminTarget)
    private val charactersProxy = buildProxy(httpClient, charactersTarget)
    private val inventoryProxy = buildProxy(httpClient, inventoryTarget)

    @Route(regex = "/admin/.*")
    fun admin(ctx: RoutingContext): Unit = forward(adminProxy, ctx)

    @Route(regex = "/characters/.*")
    fun characters(ctx: RoutingContext): Unit = forward(charactersProxy, ctx)

    @Route(regex = "/inventory/.*")
    fun inventory(ctx: RoutingContext): Unit = forward(inventoryProxy, ctx)

    private fun forward(proxy: HttpProxy, ctx: RoutingContext) {
        if (!roleConfig.isActive("gateway")) {
            ctx.next()
            return
        }
        proxy.handle(ctx.request())
    }

    private companion object {
        /** Splits a `host:port` config value the same way [RoutedBackend] does for the QUIC targets. */
        fun buildProxy(client: io.vertx.core.http.HttpClient, target: String): HttpProxy {
            val idx = target.lastIndexOf(':')
            require(idx > 0 && idx < target.length - 1) { "gateway http target must be host:port, got '$target'" }
            val host = target.substring(0, idx)
            val port = target.substring(idx + 1).toInt()
            return HttpProxy.reverseProxy(client).origin(port, host)
        }
    }
}
