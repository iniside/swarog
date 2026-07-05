package admin

import admin.adminapi.AdminItemDto
import jakarta.ws.rs.GET
import jakarta.ws.rs.Path
import jakarta.ws.rs.PathParam
import jakarta.ws.rs.Produces
import jakarta.ws.rs.core.MediaType

/**
 * The REST view of a remote module's `/admin-data/<id>` endpoint. Built PROGRAMMATICALLY per base
 * URI (`QuarkusRestClientBuilder`, see [AdminResource]) rather than `@RegisterRestClient`, because the
 * SAME interface fronts every module — only the base URI (`admin.<id>.url`, default
 * `stork://<id>-service`) differs. No CDI scope / no `@RegisterRestClient` = admin never binds to a
 * specific module at build time; the module list is pure runtime config.
 */
@Path("/admin-data")
interface AdminDataClient {
    @GET
    @Path("/{id}")
    @Produces(MediaType.APPLICATION_JSON)
    fun fetch(@PathParam("id") id: String): AdminItemDto
}
