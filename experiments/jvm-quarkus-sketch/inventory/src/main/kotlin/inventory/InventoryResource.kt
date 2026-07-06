package inventory

import characters.charactersapi.CharactersUnavailableException
import io.smallrye.common.annotation.Blocking
import jakarta.enterprise.context.ApplicationScoped
import jakarta.ws.rs.POST
import jakarta.ws.rs.Path
import jakarta.ws.rs.PathParam
import jakarta.ws.rs.QueryParam
import jakarta.ws.rs.core.Response

/**
 * Minimal HTTP surface that drives [InventoryModule.add] for a character owner — the ONLY caller today
 * is `Seed` (monolith, plain thread), so in a split `inventory` process there is otherwise no way to
 * exercise the edge `ownerOf` authorization. `@Blocking` is REQUIRED: `add` calls `PlayerCharacters.
 * ownerOf`, whose edge-RPC client adapter blocks on the QUIC round-trip — illegal on the Vert.x event
 * loop, so this hops to a worker thread. A rejected write (unknown character) surfaces as 400; a
 * characters provider that is unreachable surfaces as 503 ([CharactersUnavailableException]) — the two
 * are NOT conflated, so a dead upstream can't masquerade as a bad request.
 */
@Path("/inventory")
@ApplicationScoped
class InventoryResource(
    private val inventory: InventoryModule,
) {
    @POST
    @Path("/{characterId}/grant")
    @Blocking
    fun grant(
        @PathParam("characterId") characterId: Long,
        @QueryParam("item") item: String?,
        @QueryParam("qty") qty: Int?,
    ): Response =
        try {
            inventory.add(Owner(OwnerType.CHARACTER, characterId.toString()), item ?: "starter_sword", qty ?: 1)
            Response.ok().build()
        } catch (e: CharactersUnavailableException) {
            // Upstream characters provider unreachable — surface it as 503, NOT a false 400.
            Response.status(Response.Status.SERVICE_UNAVAILABLE).entity(e.message).build()
        } catch (e: IllegalStateException) {
            // The only other expected failure: InventoryModule.add()'s `error(...)` guard for an
            // unknown/unauthorized character. Narrowed from a blanket `catch (e: Exception)` — that
            // previously also swallowed unrelated failures (e.g. a persistence exception) and
            // misreported them as a client 400 instead of letting them surface as a genuine 500.
            Response.status(Response.Status.BAD_REQUEST).entity(e.message).build()
        }
}
