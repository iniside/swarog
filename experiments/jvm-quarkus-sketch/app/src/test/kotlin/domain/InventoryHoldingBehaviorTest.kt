package domain

import inventory.InventoryAdminData
import inventory.InventoryModule
import inventory.Owner
import inventory.OwnerType
import io.quarkus.arc.ClientProxy
import io.quarkus.narayana.jta.QuarkusTransaction
import io.quarkus.test.junit.QuarkusTest
import io.quarkus.test.junit.TestProfile
import jakarta.inject.Inject
import java.util.UUID
import javax.sql.DataSource
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * P1-INVENTORY-GAPS (owner-scoped write/read invariants). Uses PLAYER owners so the tests need no
 * character existence and no async create/delete fan-out (scheduler off) — every row is in the
 * `inventory` schema, scoped to owner ids this test invents, and removed in [cleanup] (DB delta-zero).
 * No global-count assertions: the admin-KPI test measures a DELTA around its own writes, since the
 * shared `jvmsketch` DB is cumulative.
 */
@QuarkusTest
@TestProfile(SchedulerDisabledProfile::class)
class InventoryHoldingBehaviorTest {

    @Inject
    lateinit var inventory: InventoryModule

    @Inject
    lateinit var adminData: InventoryAdminData

    @Inject
    lateinit var db: DataSource

    private val cleanupOwnerIds = mutableListOf<String>()

    private fun playerOwner(): Owner {
        val owner = Owner(OwnerType.PLAYER, "test-player-${UUID.randomUUID()}")
        cleanupOwnerIds += owner.id
        return owner
    }

    @AfterEach
    fun cleanup() {
        db.connection.use { c ->
            for (ownerId in cleanupOwnerIds) {
                c.prepareStatement(
                    "DELETE FROM inventory.holdings WHERE owner_type = 'PLAYER' AND owner_id = ?",
                ).use { ps ->
                    ps.setString(1, ownerId)
                    ps.executeUpdate()
                }
            }
        }
    }

    @Test
    fun `repeated grant of the SAME item on the same owner accumulates the quantity`() {
        val owner = playerOwner()

        inventory.add(owner, "elixir", 2)
        inventory.add(owner, "elixir", 3)   // same item -> qty += qty (2 + 3), not a second row

        assertEquals(listOf("elixir" to 5), inventory.holdings(owner))
    }

    @Test
    fun `holdings are returned alpha-sorted by item, not in insertion order`() {
        val owner = playerOwner()

        // Insert in reverse-alpha order; Sort.by("id.item") must return them alpha-ascending.
        inventory.add(owner, "cwand", 1)
        inventory.add(owner, "bsword", 1)
        inventory.add(owner, "aaxe", 1)

        assertEquals(listOf("aaxe" to 1, "bsword" to 1, "cwand" to 1), inventory.holdings(owner))
    }

    @Test
    fun `add for a PLAYER owner skips the ownerOf authz check and persists`() {
        // LOCK of current behavior: only a CHARACTER owner is authorized via characters.ownerOf; a
        // PLAYER owner is written with NO existence check at all. Here the owner id belongs to no real
        // player, yet the write succeeds — players are authoritative from `accounts`, so inventory does
        // not cross-module-authorize them (unlike characters). Flagged as intended, not a gap.
        val owner = playerOwner()

        inventory.add(owner, "gold", 7)

        assertEquals(listOf("gold" to 7), inventory.holdings(owner))
    }

    @Test
    fun `wipe returns the number of rows deleted, and zero for an empty owner`() {
        val owner = playerOwner()
        inventory.add(owner, "sword", 1)
        inventory.add(owner, "shield", 1)

        // wipe is private and its Long return is only logged by onCharacterDeleted; assert it directly
        // on the unwrapped bean, inside an explicit transaction (Panache delete requires one).
        val real = ClientProxy.unwrap(inventory)
        val wipeMethod = InventoryModule::class.java.getDeclaredMethod("wipe", Owner::class.java)
        wipeMethod.isAccessible = true

        val wiped = QuarkusTransaction.requiringNew().call { wipeMethod.invoke(real, owner) as Long }
        assertEquals(2L, wiped, "wipe returns the count of holdings removed")
        assertTrue(inventory.holdings(owner).isEmpty(), "the owner has no holdings after wipe")

        val emptyOwner = playerOwner()
        val wipedEmpty = QuarkusTransaction.requiringNew().call { wipeMethod.invoke(real, emptyOwner) as Long }
        assertEquals(0L, wipedEmpty, "wiping an owner with no holdings returns 0")
    }

    @Test
    fun `admin data KPIs reflect added holdings - Holdings counts rows, Owners counts distinct owners`() {
        val (holdingsBefore, ownersBefore) = readKpis()

        // Two DISTINCT items for ONE new owner: Holdings +2 (two rows), Owners +1 (one distinct owner).
        val owner = playerOwner()
        inventory.add(owner, "potion", 1)
        inventory.add(owner, "ether", 1)

        val (holdingsAfter, ownersAfter) = readKpis()
        assertEquals(2, holdingsAfter - holdingsBefore, "Holdings KPI counts each holding row")
        assertEquals(1, ownersAfter - ownersBefore, "Owners KPI counts distinct (owner_type, owner_id)")

        // The dashboard table is capped at 20 rows (Page.ofSize(20)).
        val rowCount = QuarkusTransaction.requiringNew().call { adminData.data().table?.rows?.size ?: 0 }
        assertTrue(rowCount <= 20, "the admin table is capped at 20 rows")
    }

    private fun readKpis(): Pair<Int, Int> = QuarkusTransaction.requiringNew().call {
        val kpis = adminData.data().kpis
        val holdings = kpis.single { it.label == "Holdings" }.value.toInt()
        val owners = kpis.single { it.label == "Owners" }.value.toInt()
        holdings to owners
    }
}
