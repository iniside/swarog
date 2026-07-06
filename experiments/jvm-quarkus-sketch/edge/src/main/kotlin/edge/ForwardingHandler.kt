package edge

import java.util.concurrent.TimeoutException

/**
 * Thrown by a [ForwardingHandler] when the downstream leg fails (connection dead, timed out, or the
 * downstream itself answered `ok=false`). [EdgeRouter.dispatch] catches it like any handler throw and
 * turns it into a clean `Response(ok=false, error=…)`, so a gateway relaying to a DOWN backend returns
 * a proper failure to the inbound caller — never a hang, never a leaked bare `TimeoutException`.
 */
class ForwardFailedException(message: String, cause: Throwable? = null) : RuntimeException(message, cause)

/**
 * The gateway's byte-relay handler: on an inbound request, forward the payload bytes UNCHANGED to a
 * downstream service via [EdgeClient.requestRaw] and return the downstream reply's bytes. Registered
 * on a gateway [EdgeRouter] (typically under a prefix, e.g. `characters.`), it is the forwarding
 * variant of the inventory→characters pattern — an [EdgeServer] whose handlers are [EdgeClient]s.
 *
 * Byte-relay correctness: it uses [EdgeClient.requestRaw], NOT [EdgeClient.request], so the inbound
 * payload blob reaches the downstream `typedHandler` byte-identically. Routing [request] here would
 * msgpack-bin-wrap the blob and the downstream typed decode would fail (the double-encode bug).
 *
 * Failure mapping ([budgetMs] < the inbound caller's timeout so the client sees a clean `ok=false`,
 * not a bare timeout): a dead downstream ([ConnectionClosedException]), an unanswered downstream
 * ([TimeoutException]), and a downstream `ok=false` all become a [ForwardFailedException] — which
 * [EdgeRouter.dispatch] maps to `Response(ok=false)`. Nothing hangs and nothing escapes past the
 * dispatch boundary.
 *
 * [outbound] is a supplier (not a bare [EdgeClient]) so the caller owns the client's lifecycle and
 * caching (the `CachedResource`+bounded-retry pattern from `characters-client`): the supplier can
 * hand back a freshly re-dialed client after a reconnect. A test passes `{ fakeClient }`.
 */
class ForwardingHandler(
    private val method: String,
    private val outbound: () -> EdgeClient,
    private val budgetMs: Long = 1_000,
) : EdgeHandler {

    override fun handle(payload: ByteArray): ByteArray {
        val resp = try {
            outbound().requestRaw(method, payload, budgetMs)
        } catch (e: ConnectionClosedException) {
            throw ForwardFailedException("forward '$method' failed: downstream connection closed", e)
        } catch (e: TimeoutException) {
            throw ForwardFailedException("forward '$method' failed: downstream did not answer within ${budgetMs}ms", e)
        }
        if (!resp.ok) {
            throw ForwardFailedException("forward '$method' failed: ${resp.error ?: "<no error message>"}")
        }
        return resp.payload
    }
}
