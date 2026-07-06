package inventory

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
import jakarta.persistence.EntityManager
import jakarta.ws.rs.GET
import jakarta.ws.rs.Path
import jakarta.ws.rs.Produces
import jakarta.ws.rs.core.MediaType

/**
 * Inventory's admin dashboard, Step 6 shape (was `@Produces Item { closure }` in [InventoryModule]).
 *  - THIS [AdminDataProvider] bean — discovered by admin via `@All`, called in-process when
 *    `inventory` is co-located. `data()` reads the live DB (same KPIs/table as the old closure).
 *  - [InventoryAdminDataResource] — the WIRE endpoint (`GET /admin-data/inventory`) so a REMOTE
 *    admin process fetches the identical [AdminItemDto] over REST/JSON.
 */
@ApplicationScoped
class InventoryAdminData(
    private val em: EntityManager,
) : AdminDataProvider {
    override val id = "inventory"
    override val section = "Game Content"
    override val label = "Inventory"

    override fun data(): SectionData = SectionData(
        kpis = listOf(
            Kpi("Holdings", Holding.count().toString()),
            Kpi("Owners", distinctOwners().toString()),
        ),
        table = Table(
            headers = listOf("Owner", "ID", "Item", "Qty"),
            rows = rowsByOwner(20).map { h ->
                listOf(
                    Cell(h.id.ownerType.lowercase(), badge = true),
                    Cell(h.id.ownerId, mono = true),
                    Cell(h.id.item),
                    Cell(h.qty.toString(), mono = true),
                )
            },
        ),
    )

    /** count(distinct composite-key-prefix) has no JPQL spelling — the one query that stays SQL. */
    private fun distinctOwners(): Long =
        (em.createNativeQuery("SELECT count(*) FROM (SELECT DISTINCT owner_type, owner_id FROM inventory.holdings) t")
            .singleResult as Number).toLong()

    /** The first [limit] holdings ordered by (owner_id, item) — NOT temporal. There is no timestamp
     *  column on `holdings`, so this is a deterministic owner-grouped slice for the dashboard, not a
     *  "most recent" feed (the previous `recentRows` name was a misnomer). */
    private fun rowsByOwner(limit: Int): List<Holding> =
        Holding.findAll(Sort.by("id.ownerId").and("id.item")).page(Page.ofSize(limit)).list()
}

/**
 * Exposes [InventoryAdminData] on the wire. `@Blocking` because `data()` hits Postgres (Panache is
 * blocking). Present only in a process that hosts `inventory`.
 */
@Path("/admin-data/inventory")
@ApplicationScoped
class InventoryAdminDataResource(
    private val provider: InventoryAdminData,
) {
    @GET
    @Produces(MediaType.APPLICATION_JSON)
    @Blocking
    fun get(): AdminItemDto = AdminItemDto(provider.id, provider.section, provider.label, provider.data())
}
