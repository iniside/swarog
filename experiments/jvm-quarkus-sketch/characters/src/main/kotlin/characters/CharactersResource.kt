package characters

import io.smallrye.common.annotation.Blocking
import jakarta.enterprise.context.ApplicationScoped
import jakarta.ws.rs.DELETE
import jakarta.ws.rs.POST
import jakarta.ws.rs.Path
import jakarta.ws.rs.PathParam
import jakarta.ws.rs.Produces
import jakarta.ws.rs.QueryParam
import jakarta.ws.rs.core.MediaType
import java.util.UUID

/**
 * The DRIVER surface for the characters domain: create/delete a character over HTTP so split
 * process A (the `characters` role) can be exercised end-to-end. The demo [Seed] is gated to the
 * monolith (`roles=all`), so in microservices mode this REST endpoint is the ONLY way to drive the
 * create → outbox HTTP fanout → inventory-grant flow.
 *
 * Deliberately tiny: it just forwards to [CharactersModule]. `@Blocking` because create/delete do
 * JPA writes (Panache is blocking — illegal on the Vert.x event loop, so this hops to a worker).
 * Present only in a process that hosts `characters` (its @Path bean rides the classpath there).
 */
@Path("/characters")
@ApplicationScoped
class CharactersResource(
    private val characters: CharactersModule,
) {
    /**
     * Create a character. `name` is required (query or form). `playerId` is optional — when absent a
     * random UUID stands in, since `player_id` is a plain column with no cross-module FK to accounts.
     * Returns the new BIGSERIAL id as text.
     */
    @POST
    @Blocking
    @Produces(MediaType.TEXT_PLAIN)
    fun create(
        @QueryParam("name") name: String?,
        @QueryParam("playerId") playerId: String?,
    ): String {
        val resolvedName = name ?: "unnamed"
        val owner = playerId?.let(UUID::fromString) ?: UUID.randomUUID()
        return characters.create(owner, resolvedName).toString()
    }

    /** Delete a character by id. No-op if it does not exist ([CharactersModule.delete] handles that). */
    @DELETE
    @Path("/{id}")
    @Blocking
    fun delete(@PathParam("id") id: Long) {
        characters.delete(id)
    }
}
