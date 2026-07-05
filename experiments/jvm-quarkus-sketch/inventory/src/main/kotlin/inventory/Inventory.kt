package inventory

import admin.adminapi.Cell
import admin.adminapi.Item
import admin.adminapi.Kpi
import admin.adminapi.SectionData
import admin.adminapi.Table
import characters.charactersapi.PlayerCharacters
import characters.charactersevents.CharacterCreated
import characters.charactersevents.CharacterDeleted
import io.quarkus.panache.common.Page
import io.quarkus.panache.common.Sort
import io.quarkus.runtime.StartupEvent
import jakarta.enterprise.context.ApplicationScoped
import jakarta.enterprise.event.Observes
import jakarta.enterprise.event.ObservesAsync
import jakarta.enterprise.inject.Produces
import jakarta.persistence.EntityManager
import jakarta.transaction.Transactional
import javax.sql.DataSource

enum class OwnerType { PLAYER, CHARACTER }

/** Polymorphic owner. `id` is a plain ref — for a character it's the character id as text. */
data class Owner(val type: OwnerType, val id: String)

/**
 * Owner-scoped holdings. Depends on accounts + characters.
 *  - SYNC-asks [PlayerCharacters.ownerOf] to authorize a character's inventory — the capability
 *    interface is constructor-injected BY TYPE; no provide/require, no package dependency on impl.
 *  - REACTS to character events via `@ObservesAsync`: grant a starter item on create, wipe
 *    holdings on delete. `characters` has no idea this module exists.
 *
 * Persistence: Panache over the [Holding] entity. The JDBC ladder collapsed to one-liners;
 * the new costs are `@Transactional` on every write path and one query that stayed native
 * (count-distinct over the composite key).
 */
@ApplicationScoped
class InventoryModule(
    private val db: DataSource,
    private val em: EntityManager,
    private val characters: PlayerCharacters,
) {

    /** Own schema only, raw DDL — migrations stay the module's property, the ORM just maps the
     *  result (`quarkus.hibernate-orm` schema management is off). Default priority:
     *  order-independent by construction (own schema, no cross-module FKs). */
    fun migrate(@Observes ev: StartupEvent) {
        db.connection.use { c ->
            c.createStatement().use { s ->
                s.execute("CREATE SCHEMA IF NOT EXISTS inventory")
                s.execute(
                    """CREATE TABLE IF NOT EXISTS inventory.holdings(
                        owner_type TEXT NOT NULL,
                        owner_id   TEXT NOT NULL,   -- plain polymorphic ref; NO cross-module FK
                        item       TEXT NOT NULL,
                        qty        INT  NOT NULL,
                        PRIMARY KEY (owner_type, owner_id, item))"""
                )
            }
        }
        println("[inventory] schema ready")
    }

    /** Sideways reactions — `@ObservesAsync` receives `fireAsync` deliveries off the caller's thread. */
    @Transactional
    fun onCharacterCreated(@ObservesAsync ev: CharacterCreated) {
        grant(Owner(OwnerType.CHARACTER, ev.characterId.toString()), "starter_sword", 1)
        println("  [inventory] granted starter_sword to character ${ev.characterId}")
    }

    @Transactional
    fun onCharacterDeleted(@ObservesAsync ev: CharacterDeleted) {
        val wiped = wipe(Owner(OwnerType.CHARACTER, ev.characterId.toString()))
        println("  [inventory] wiped $wiped holding(s) for deleted character ${ev.characterId}")
    }

    /** Second contributor to the same Item "slot" — admin merges both, still without knowing us. */
    @Produces
    @ApplicationScoped
    fun inventoryAdminItem(): Item = Item(section = "Game Content", label = "Inventory") {
        SectionData(
            kpis = listOf(
                Kpi("Holdings", Holding.count().toString()),
                Kpi("Owners", distinctOwners().toString()),
            ),
            table = Table(
                headers = listOf("Owner", "ID", "Item", "Qty"),
                rows = recentRows(20).map { h ->
                    listOf(
                        Cell(h.id.ownerType.lowercase(), badge = true),
                        Cell(h.id.ownerId, mono = true),
                        Cell(h.id.item),
                        Cell(h.qty.toString(), mono = true),
                    )
                },
            ),
        )
    }

    /** Authorizes a character inventory by SYNC-asking the capability — not the package. */
    @Transactional
    fun add(owner: Owner, item: String, qty: Int) {
        if (owner.type == OwnerType.CHARACTER && characters.ownerOf(owner.id.toLong()) == null) {
            error("no such character ${owner.id} — refusing inventory write")
        }
        grant(owner, item, qty)
    }

    fun holdings(owner: Owner): List<Pair<String, Int>> =
        Holding.find("id.ownerType = ?1 and id.ownerId = ?2", Sort.by("id.item"), owner.type.name, owner.id)
            .list().map { it.id.item to it.qty }

    /** The SQL upsert (ON CONFLICT .. DO UPDATE) became load-modify: mutate the managed entity and
     *  Hibernate's dirty checking writes the UPDATE — no explicit save call anywhere. */
    private fun grant(owner: Owner, item: String, qty: Int) {
        val id = HoldingId(owner.type.name, owner.id, item)
        Holding.findById(id)?.let { it.qty += qty } ?: Holding(id, qty).persist()
    }

    private fun wipe(owner: Owner): Long =
        Holding.delete("id.ownerType = ?1 and id.ownerId = ?2", owner.type.name, owner.id)

    /** count(distinct composite-key-prefix) has no JPQL spelling — the one query that stayed SQL. */
    private fun distinctOwners(): Long =
        (em.createNativeQuery("SELECT count(*) FROM (SELECT DISTINCT owner_type, owner_id FROM inventory.holdings) t")
            .singleResult as Number).toLong()

    private fun recentRows(limit: Int): List<Holding> =
        Holding.findAll(Sort.by("id.ownerId").and("id.item")).page(Page.ofSize(limit)).list()
}
