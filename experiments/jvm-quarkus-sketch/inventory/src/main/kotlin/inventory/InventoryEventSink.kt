package inventory

import characters.charactersevents.CharacterCreated
import characters.charactersevents.CharacterDeleted
import io.smallrye.common.annotation.Blocking
import jakarta.enterprise.context.ApplicationScoped
import jakarta.ws.rs.Consumes
import jakarta.ws.rs.POST
import jakarta.ws.rs.Path
import jakarta.ws.rs.core.MediaType
import jakarta.ws.rs.core.Response

/**
 * Async event subscriber, broker-less: character events arrive as a DIRECT HTTP POST from the
 * characters outbox relay (JSON body = the published payload). This is the SAME cross-process
 * transport already used by the sync edge ownerOf and the admin-data REST fan-out — one HTTP hop,
 * no broker, no messaging channel.
 *
 * @Blocking: the delegated handlers do JPA writes, so they must run on a worker thread, not the I/O
 * event loop. The idempotent dedup + effect stay atomic inside [InventoryModule]'s @Transactional
 * handlers, so a redelivered POST (the relay retries any non-2xx) is a safe no-op.
 */
@Path("/events")
@ApplicationScoped
class InventoryEventSink(private val inventory: InventoryModule) {

    @POST
    @Path("/character-created")
    @Blocking
    @Consumes(MediaType.APPLICATION_JSON)
    fun characterCreated(event: CharacterCreated): Response {
        inventory.onCharacterCreated(event)
        return Response.ok().build()
    }

    @POST
    @Path("/character-deleted")
    @Blocking
    @Consumes(MediaType.APPLICATION_JSON)
    fun characterDeleted(event: CharacterDeleted): Response {
        inventory.onCharacterDeleted(event)
        return Response.ok().build()
    }
}
