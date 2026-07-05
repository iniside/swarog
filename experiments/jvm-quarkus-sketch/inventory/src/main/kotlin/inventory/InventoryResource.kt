package inventory

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
 * exercise the gRPC `ownerOf` authorization. `@Blocking` is REQUIRED: `add` calls `PlayerCharacters.
 * ownerOf`, whose gRPC adapter blocks on `.await().indefinitely()` — illegal on the Vert.x event loop,
 * so this hops to a worker thread. A rejected write (unknown character) surfaces as 400.
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
        } catch (e: Exception) {
            Response.status(Response.Status.BAD_REQUEST).entity(e.message).build()
        }
}
