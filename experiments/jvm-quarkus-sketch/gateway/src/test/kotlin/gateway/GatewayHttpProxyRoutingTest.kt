package gateway

import io.vertx.core.Vertx
import io.vertx.core.http.HttpMethod
import io.vertx.ext.web.Router
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.Timeout
import platform.RoleConfig
import java.util.concurrent.TimeUnit

/**
 * Proves the HTTP reverse-proxy side (Step 4) forwards the FULL inbound path verbatim — not just a
 * 200 (reviewer #9). Exercises the REAL [GatewayHttpProxy] route handlers (the exact methods CDI would
 * dispatch `@Route` to) against a real Vert.x front door and a real Vert.x stub backend, over loopback
 * TCP — no Quarkus boot, no msquic/QUIC/native cert needed (the HTTP side is independent of the QUIC
 * side's [edge.server.cert-thumbprint][GatewayEdgeServer] requirement, which a full `@QuarkusTest` of
 * `gateway-service` cannot avoid since `roles=gateway` there would also arm the QUIC listener — see the
 * class doc on why this is the achievable proof instead).
 *
 * Front door: a plain `vertx-web` [Router] wired to call [GatewayHttpProxy.admin] / `.characters` /
 * `.inventory` directly for each prefix — the SAME handler methods CDI's `@Route` machinery would
 * invoke, just dispatched by a hand-built router instead of Quarkus augmentation. The stub backend
 * records the request path/method it actually received, so a passing assertion proves the proxy hop
 * preserves the path byte-for-byte (`/admin/foo/bar` arrives at the origin as `/admin/foo/bar`), not
 * merely that *some* response came back.
 */
class GatewayHttpProxyRoutingTest {

    private val vertx: Vertx = Vertx.vertx()

    @AfterEach
    fun tearDown() {
        vertx.close().toCompletionStage().toCompletableFuture().get(10, TimeUnit.SECONDS)
    }

    /** A stub origin server: records the request path + method it received, replies 200. */
    private fun startStubBackend(): Pair<Int, MutableList<Pair<HttpMethod, String>>> {
        val received = mutableListOf<Pair<HttpMethod, String>>()
        val server = vertx.createHttpServer().requestHandler { req ->
            synchronized(received) { received += req.method() to req.path() }
            req.response().setStatusCode(200).end("backend-ok")
        }
        val port = server.listen(0).toCompletionStage().toCompletableFuture().get(10, TimeUnit.SECONDS).actualPort()
        return port to received
    }

    /** The front door: a vertx-web Router wired exactly like Quarkus's @Route would dispatch. */
    private fun startFrontDoor(proxy: GatewayHttpProxy): Int {
        val router = Router.router(vertx)
        router.route("/admin/*").handler(proxy::admin)
        router.route("/characters/*").handler(proxy::characters)
        router.route("/inventory/*").handler(proxy::inventory)
        val server = vertx.createHttpServer().requestHandler(router)
        return server.listen(0).toCompletionStage().toCompletableFuture().get(10, TimeUnit.SECONDS).actualPort()
    }

    private fun get(port: Int, path: String): Int {
        val client = vertx.createHttpClient()
        val response = client.request(HttpMethod.GET, port, "localhost", path)
            .toCompletionStage().toCompletableFuture().get(10, TimeUnit.SECONDS)
            .send()
            .toCompletionStage().toCompletableFuture().get(10, TimeUnit.SECONDS)
        val status = response.statusCode()
        client.close().toCompletionStage().toCompletableFuture().get(10, TimeUnit.SECONDS)
        return status
    }

    @Test
    @Timeout(value = 15, unit = TimeUnit.SECONDS, threadMode = Timeout.ThreadMode.SEPARATE_THREAD)
    fun `each prefix proxies to its OWN backend, preserving the full path verbatim`() {
        val (adminPort, adminReceived) = startStubBackend()
        val (charsPort, charsReceived) = startStubBackend()
        val (invPort, invReceived) = startStubBackend()

        val proxy = GatewayHttpProxy(
            vertx = vertx,
            roleConfig = RoleConfig(setOf("gateway")),
            adminTarget = "localhost:$adminPort",
            charactersTarget = "localhost:$charsPort",
            inventoryTarget = "localhost:$invPort",
        )
        val frontPort = startFrontDoor(proxy)

        assertEquals(200, get(frontPort, "/admin/foo/bar"))
        assertEquals(200, get(frontPort, "/characters/42"))
        assertEquals(200, get(frontPort, "/inventory/7/grant"))

        // The path the ORIGIN actually received must be the full inbound path, verbatim — not a
        // rewrite, not truncated at the prefix.
        assertEquals(listOf(HttpMethod.GET to "/admin/foo/bar"), adminReceived)
        assertEquals(listOf(HttpMethod.GET to "/characters/42"), charsReceived)
        assertEquals(listOf(HttpMethod.GET to "/inventory/7/grant"), invReceived)
    }

    @Test
    @Timeout(value = 15, unit = TimeUnit.SECONDS, threadMode = Timeout.ThreadMode.SEPARATE_THREAD)
    fun `the role gate declines to forward when this process is not the gateway`() {
        val (adminPort, adminReceived) = startStubBackend()

        val proxy = GatewayHttpProxy(
            vertx = vertx,
            roleConfig = RoleConfig(setOf("some-other-role")),
            adminTarget = "localhost:$adminPort",
            charactersTarget = "localhost:$adminPort",
            inventoryTarget = "localhost:$adminPort",
        )
        val frontPort = startFrontDoor(proxy)

        // ctx.next() falls through to no match => vertx-web's default 404, and the backend never sees
        // a request.
        assertEquals(404, get(frontPort, "/admin/foo"))
        assertEquals(emptyList<Pair<HttpMethod, String>>(), adminReceived)
    }
}
