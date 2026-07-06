package gateway

import edge.CachedResource
import edge.EdgeClient
import edge.EdgeConnection
import edge.EdgeRouter
import edge.ForwardFailedException
import edge.Response
import edge.msquic.MsQuicClientTransport

/**
 * A cached, self-reconnecting outbound leg to ONE downstream service, exposed as a method-transparent
 * [EdgeRouter.MethodForward]. This is the forwarding variant of the inventory→characters pattern — an
 * [EdgeServer] handler that is itself an [EdgeClient] — and it deliberately MIRRORS
 * `charactersclient.EdgeRemotePlayerCharacters`: a retained [MsQuicClientTransport] built EXACTLY ONCE
 * (a fresh one per reconnect would leak a native registration), a [CachedResource] holding the QUIC
 * connection + [EdgeClient] dialed lazily on the first forward and reused, dropped + re-dialed ONCE on
 * failure (bounded: exactly one retry, so a dead peer cannot cause a reconnect storm).
 *
 * Byte-relay correctness: it forwards the inbound [method] + payload UNCHANGED via
 * [EdgeClient.requestRaw] (NOT `request`, which would msgpack-bin-wrap the already-encoded blob and
 * break the downstream typed decode). Because it is a [MethodForward], it forwards the ORIGINAL method,
 * so one `characters.` registration correctly relays `characters.list`, `characters.ownerOf`, … each
 * under its own name.
 *
 * Failure mapping: both attempts failing, or a downstream `ok=false`, becomes a [ForwardFailedException]
 * — which [EdgeRouter.dispatch] maps to `Response(ok=false)`. So a DOWN backend is a clean failure to
 * the inbound player, never a hang. [budgetMs] is deliberately SHORTER than an inbound caller's timeout
 * so the player sees `ok=false` rather than its own bare timeout.
 *
 * cid safety (why relaying to a shared outbound client is fine): the outbound [EdgeClient] mints a FRESH
 * cid on its own concurrent-safe `pending` map; [EdgeRouter.dispatch] re-wraps the reply in
 * `Response(inboundCid, …)`. Independent cid spaces per hop → no collision even when many inbound
 * connections share this one cached outbound client.
 */
internal class RoutedBackend(
    target: String,
    private val budgetMs: Long = DEFAULT_BUDGET_MS,
    connect: ((String, Int) -> EdgeConnection)? = null,
) : EdgeRouter.MethodForward {

    private val host: String
    private val port: Int

    init {
        val idx = target.lastIndexOf(':')
        require(idx > 0 && idx < target.length - 1) { "gateway route target must be host:port, got '$target'" }
        host = target.substring(0, idx)
        port = target.substring(idx + 1).toInt()
    }

    // Retained native transport: constructed once, reused across reconnects (see class doc).
    private val transport = MsQuicClientTransport()

    // Dial boundary, injected for testability: production binds it to the retained transport; a unit
    // test passes a fake returning an in-memory loopback connection. Nullable (not a default referencing
    // `transport`) because a ctor default cannot reference the body-declared field before it inits.
    private val connect: (String, Int) -> EdgeConnection = connect ?: transport::connect

    private val cache = CachedResource<EdgeSession>(
        create = {
            val conn = connect(host, port)
            EdgeSession(conn, EdgeClient(conn).apply { start() })
        },
        close = { it.connection.close() },
    )

    @Suppress("TooGenericExceptionCaught") // deliberately broad (mirrors EdgeRemotePlayerCharacters): the
    // outbound edge client's failure modes (dead connection, timed-out call, native error) are all
    // unchecked and untyped by design — the bounded-retry contract must trigger on ANY of them.
    override fun forward(method: String, payload: ByteArray): ByteArray {
        val resp = try {
            callOnce(method, payload)
        } catch (e: Exception) {
            // The connection is likely dead (peer restarted / stream closed / call timed out). Drop the
            // cached client and reconnect ONCE, then retry.
            cache.invalidate()
            try {
                callOnce(method, payload)
            } catch (retry: Exception) {
                // Keep the FIRST attempt's cause (suppressed) so the original failure reason survives.
                retry.addSuppressed(e)
                throw ForwardFailedException("forward '$method' to $host:$port failed", retry)
            }
        }
        if (!resp.ok) {
            throw ForwardFailedException("forward '$method' failed: ${resp.error ?: "<no error message>"}")
        }
        return resp.payload
    }

    private fun callOnce(method: String, payload: ByteArray): Response =
        cache.get().client.requestRaw(method, payload, budgetMs)

    /** Drops the cached connection (best-effort close) — called on gateway shutdown. */
    fun close() {
        cache.invalidate()
    }

    private companion object {
        const val DEFAULT_BUDGET_MS = 1_000L
    }
}

/** The cached unit: one QUIC [EdgeConnection] and the [EdgeClient] driving it. Invalidation closes the connection. */
internal class EdgeSession(val connection: EdgeConnection, val client: EdgeClient)
