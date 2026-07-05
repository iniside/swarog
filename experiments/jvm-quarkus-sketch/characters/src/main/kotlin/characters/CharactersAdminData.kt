package characters

import admin.adminapi.AdminDataProvider
import admin.adminapi.AdminItemDto
import admin.adminapi.Cell
import admin.adminapi.Kpi
import admin.adminapi.SectionData
import admin.adminapi.Table
import io.quarkus.panache.common.Page
import io.quarkus.panache.common.Sort
import io.smallrye.common.annotation.Blocking
import jakarta.enterprise.context.ApplicationScoped
import jakarta.ws.rs.GET
import jakarta.ws.rs.Path
import jakarta.ws.rs.Produces
import jakarta.ws.rs.core.MediaType

/**
 * The admin contribution seam, Step 6 shape. The old `@Produces Item { closure }` split into:
 *  - THIS [AdminDataProvider] bean — a LOCAL capability the admin discovers via `@All` and calls
 *    in-process when `characters` is co-located. `data()` reads the live DB (same numbers/table as
 *    the old closure produced).
 *  - [CharactersAdminDataResource] — the WIRE endpoint (`GET /admin-data/characters`) so a REMOTE
 *    admin process can fetch the identical [AdminItemDto] over REST/JSON.
 *
 * Identity is fixed here (id/section/label) so both halves agree; presentation ORDER is still the
 * admin's business (it sorts by section, label).
 */
@ApplicationScoped
class CharactersAdminData : AdminDataProvider {
    override val id = "characters"
    override val section = "Game Content"
    override val label = "Characters"

    override fun data(): SectionData = SectionData(
        kpis = listOf(Kpi("Characters", Character.count().toString())),
        table = Table(
            headers = listOf("ID", "Player", "Name"),
            rows = recent(10).map { ch ->
                listOf(Cell(ch.id.toString(), mono = true), Cell(ch.playerId.toString(), mono = true), Cell(ch.name))
            },
        ),
    )

    private fun recent(limit: Int): List<Character> =
        Character.findAll(Sort.descending("id")).page(Page.ofSize(limit)).list()
}

/**
 * Exposes [CharactersAdminData] on the wire. `@Blocking` because `data()` hits Postgres (Panache is
 * blocking) — illegal on the Vert.x event loop, so this hops to a worker thread. Present only in a
 * process that hosts `characters`; the split profile leaves it here and the `inventory`/admin process
 * reaches it via `stork://characters-service`.
 */
@Path("/admin-data/characters")
@ApplicationScoped
class CharactersAdminDataResource(
    private val provider: CharactersAdminData,
) {
    @GET
    @Produces(MediaType.APPLICATION_JSON)
    @Blocking
    fun get(): AdminItemDto = AdminItemDto(provider.id, provider.section, provider.label, provider.data())
}
