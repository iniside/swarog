package characters

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
import platform.RoleConfig

/**
 * The ONE place a [PlayerCharacters] bean comes from — transport-transparent by construction:
 *  - a process that HOSTS `characters` gets a local delegate straight to [LocalPlayerCharacters];
 *  - a process that does NOT gets an edge-RPC adapter dialing the remote QUIC server → [CharactersEdgeServer].
 *
 * Living in the `characters` impl (not `platform`/`inventory`) is deliberate: it keeps the choice next
 * to the capability it fronts and avoids an impl-on-impl / Gradle cycle. Exactly one bean of type
 * [PlayerCharacters] exists in every role combination — [LocalPlayerCharacters] is a concrete type
 * (not a `PlayerCharacters`), so no ambiguous resolution. The remote adapter connects lazily on first
 * call, so it is constructed but inert in the monolith, where the local branch is taken.
 */
@ApplicationScoped
class PlayerCharactersProvider(
    private val roleConfig: RoleConfig,
    private val local: LocalPlayerCharacters,
    @param:ConfigProperty(name = "edge.client.characters.target") private val target: String,
) {
    @Produces
    @ApplicationScoped
    fun playerCharacters(): PlayerCharacters =
        if (roleConfig.isActive("characters")) LocalPlayerCharactersAdapter(local)
        else EdgeRemotePlayerCharacters(target)
}

/** In-process: delegate straight to the concrete local capability. */
private class LocalPlayerCharactersAdapter(
    private val local: LocalPlayerCharacters,
) : PlayerCharacters {
    override fun ownerOf(characterId: Long): UUID? = local.ownerOf(characterId)
}

/**
 * Cross-process: call `characters.ownerOf` over the edge RPC core on REAL QUIC to the remote
 * [CharactersEdgeServer] at `host:port` ([target]). The [EdgeClient] blocks on the round-trip, so
 * callers that reach a character inventory write must be `@Blocking` (e.g. [inventory.InventoryResource]).
 *
 * The QUIC connection + [EdgeClient] are established lazily and CACHED (a single persistent bidi
 * stream). If a call fails because the connection died (peer restarted, stream closed → the reader
 * unblocks and pending calls time out, or a send fails), the cached client is dropped and one
 * reconnect + retry is attempted. Bounded: exactly one retry per call, so a dead peer cannot cause a
 * reconnect storm — the second failure propagates and surfaces as a 400 at the inventory write.
 */
private class EdgeRemotePlayerCharacters(target: String) : PlayerCharacters {

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

    override fun ownerOf(characterId: Long): UUID? {
        val reply = try {
            ensureClient().call("characters.ownerOf", OwnerOfRequest(characterId), OwnerOfReply::class.java)
        } catch (e: Exception) {
            // The connection is likely dead (peer restarted / stream closed / call timed out). Drop the
            // cached client and reconnect ONCE, then retry. A second failure propagates to the caller.
            invalidate()
            ensureClient().call("characters.ownerOf", OwnerOfRequest(characterId), OwnerOfReply::class.java)
        }
        return reply.ownerId?.let(UUID::fromString)
    }

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
