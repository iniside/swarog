package charactersclient

import characters.charactersapi.CharactersUnavailableException
import characters.charactersapi.OwnerOfReply
import characters.charactersapi.OwnerOfRequest
import characters.charactersapi.PlayerCharacters
import edge.EdgeClient
import edge.EdgeConnection
import edge.msquic.MsQuicClientTransport
import jakarta.enterprise.context.ApplicationScoped
import jakarta.enterprise.inject.Produces
import java.util.UUID
import org.eclipse.microprofile.config.inject.ConfigProperty

/**
 * The REMOTE [PlayerCharacters] producer. Lives in `characters-client` — a module that imports ONLY
 * `characters-api` + `edge`, NEVER the `characters` impl — so a service can depend on the remote
 * capability WITHOUT linking the characters implementation. This is the CDI crux of the per-service
 * split: previously one provider in the `characters` impl branched local-vs-remote on RoleConfig at
 * runtime; now bean PRESENCE decides. A process that includes `characters-client` (and NOT `characters`)
 * gets exactly this remote producer; a process that includes `characters` gets the LOCAL producer
 * ([characters.LocalPlayerCharactersProducer]); the monolith includes `characters` only. So each of the
 * three topologies has EXACTLY ONE `PlayerCharacters` producer — no ambiguous/unsatisfied resolution.
 *
 * The remote adapter connects lazily on first call, so it is constructed but inert until an inventory
 * write actually reaches a character-owned holding.
 */
@ApplicationScoped
class CharactersClientProducer(
    @param:ConfigProperty(name = "edge.client.characters.target") private val target: String,
) {
    @Produces
    @ApplicationScoped
    fun playerCharacters(): PlayerCharacters = EdgeRemotePlayerCharacters(target)
}

/**
 * Cross-process: call `characters.ownerOf` over the edge RPC core on REAL QUIC to the remote
 * `CharactersEdgeServer` at `host:port` ([target]). The [EdgeClient] blocks on the round-trip, so
 * callers that reach a character inventory write must be `@Blocking` (e.g. inventory's REST resource).
 *
 * The QUIC connection + [EdgeClient] are established lazily and CACHED (a single persistent bidi
 * stream). If a call fails because the connection died (peer restarted, stream closed → the reader
 * unblocks and pending calls time out, or a send fails), the cached client is dropped and one
 * reconnect + retry is attempted. Bounded: exactly one retry per call, so a dead peer cannot cause a
 * reconnect storm — a second failure means the provider is unreachable, surfaced as a distinct
 * [CharactersUnavailableException] (→ 503 at the inventory write), NOT null and NOT a false 400.
 */
internal class EdgeRemotePlayerCharacters(target: String) : PlayerCharacters {

    private val host: String
    private val port: Int

    init {
        val idx = target.lastIndexOf(':')
        require(idx > 0 && idx < target.length - 1) { "edge.client.characters.target must be host:port, got '$target'" }
        host = target.substring(0, idx)
        port = target.substring(idx + 1).toInt()
    }

    private val transport = MsQuicClientTransport()
    private val lock = Any()

    @Volatile private var connection: EdgeConnection? = null
    @Volatile private var client: EdgeClient? = null

    @Suppress("TooGenericExceptionCaught") // deliberately broad: the edge-RPC client's failure modes
    // (dead connection, timed-out call, native-transport error) are all unchecked and untyped by
    // design — the bounded-retry contract must trigger on ANY of them, not an enumerated subset.
    override fun ownerOf(characterId: Long): UUID? {
        val reply = try {
            callOnce(characterId)
        } catch (e: Exception) {
            // The connection is likely dead (peer restarted / stream closed / call timed out). Drop the
            // cached client and reconnect ONCE, then retry.
            invalidate()
            try {
                callOnce(characterId)
            } catch (retry: Exception) {
                // Both attempts failed → the provider is unreachable. Signal that DISTINCTLY (not null,
                // which means "no such character"), so the consumer maps it to 503, never a false 400.
                // Keep the FIRST attempt's exception too (as suppressed) — without it, the original
                // failure reason is lost and only the post-reconnect retry's (often different) failure
                // is visible, which is misleading when debugging the actual root cause.
                retry.addSuppressed(e)
                throw CharactersUnavailableException("characters.ownerOf unreachable at $host:$port", retry)
            }
        }
        return reply.ownerId?.let(UUID::fromString)
    }

    private fun callOnce(characterId: Long): OwnerOfReply =
        ensureClient().call("characters.ownerOf", OwnerOfRequest(characterId), OwnerOfReply::class.java)

    /** Lazily establishes (and caches) the QUIC connection + [EdgeClient]. Synchronized double-check. */
    private fun ensureClient(): EdgeClient {
        client?.let { return it }
        synchronized(lock) {
            client?.let { return it }
            val conn = transport.connect(host, port)
            val c = EdgeClient(conn).apply { start() }
            connection = conn
            client = c
            return c
        }
    }

    /** Drops the cached connection so the next [ensureClient] reconnects. Best-effort close of the old one. */
    private fun invalidate() {
        synchronized(lock) {
            connection?.let { runCatching { it.close() } }
            connection = null
            client = null
        }
    }
}
