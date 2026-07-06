package domain

import characters.charactersevents.CharacterCreated
import characters.charactersevents.CharacterDeleted
import inventory.InventoryModule
import inventory.Owner
import inventory.OwnerType
import io.quarkus.narayana.jta.QuarkusTransaction
import io.quarkus.test.junit.QuarkusTest
import io.quarkus.test.junit.TestProfile
import jakarta.inject.Inject
import jakarta.persistence.EntityManager
import java.util.UUID
import java.util.concurrent.atomic.AtomicLong
import javax.sql.DataSource
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * P1-INVENTORY-GAPS (the idempotency inbox). Two invariants of the at-least-once event handlers,
 * exercised directly against the wired [InventoryModule] with a real Postgres (scheduler off, so no
 * relay races these hand-invoked handlers):
 *  - `onCharacterDeleted` redelivery is deduped: the SECOND delivery of the same event is a no-op
 *    (symmetric to the existing created-redelivery test), proven by a holding added AFTER the first
 *    delivery surviving the second.
 *  - `firstSeen`/effect rollback coupling: the inbox row and the grant commit ATOMICALLY — a failure
 *    in the handler's transaction leaves NO inbox row behind, so the next redelivery reprocesses.
 *
 * Holdings for a CHARACTER owner are seeded by raw SQL against a SYNTHETIC character id (no such
 * character exists), which is exactly what the wipe/grant handlers key on and avoids standing up the
 * async create pipeline. Every row is id-scoped and removed in [cleanup].
 */
@QuarkusTest
@TestProfile(SchedulerDisabledProfile::class)
class InventoryInboxCouplingTest {

    @Inject
    lateinit var inventory: InventoryModule

    @Inject
    lateinit var db: DataSource

    @Inject
    lateinit var em: EntityManager

    private val idSeq = AtomicLong(System.nanoTime())
    private val cleanupCharIds = mutableListOf<Long>()

    private fun freshCharId(): Long = idSeq.getAndIncrement().also { cleanupCharIds += it }

    @AfterEach
    fun cleanup() {
        db.connection.use { c ->
            for (id in cleanupCharIds) {
                c.prepareStatement(
                    "DELETE FROM inventory.holdings WHERE owner_type = 'CHARACTER' AND owner_id = ?",
                ).use { ps ->
                    ps.setString(1, id.toString())
                    ps.executeUpdate()
                }
                c.prepareStatement("DELETE FROM inventory.inbox WHERE event_id LIKE ?").use { ps ->
                    ps.setString(1, "%:$id")
                    ps.executeUpdate()
                }
            }
        }
    }

    @Test
    fun `a redelivered character-deleted event wipes only once - the second delivery is a no-op`() {
        val charId = freshCharId()
        val owner = Owner(OwnerType.CHARACTER, charId.toString())
        val event = CharacterDeleted(charId, UUID.randomUUID())

        seedHolding(charId, "loot", 3)
        inventory.onCharacterDeleted(event)   // first delivery: firstSeen -> wipe
        assertTrue(inventory.holdings(owner).isEmpty(), "the first delivery wipes the owner's holdings")

        // New loot arrives AFTER the first delivery; a NON-deduped second delivery would wipe it too.
        seedHolding(charId, "loot2", 1)
        inventory.onCharacterDeleted(event)   // redelivery: firstSeen == false -> wipe NEVER runs

        assertEquals(
            listOf("loot2" to 1),
            inventory.holdings(owner),
            "the deduped redelivery must NOT re-wipe holdings added after the first delivery",
        )
    }

    @Test
    fun `a failure in the created-handler transaction leaves NO inbox row and the retry reprocesses`() {
        val charId = freshCharId()
        val owner = Owner(OwnerType.CHARACTER, charId.toString())
        val event = CharacterCreated(charId, UUID.randomUUID(), "RollbackHero")
        val key = "${CharacterCreated.TOPIC}:$charId"

        // onCharacterCreated is @Transactional(REQUIRED): called inside this enclosing transaction it
        // JOINS it, so its inbox INSERT + starter grant run here. We then force a failure in the SAME
        // transaction (a duplicate of the key the handler just inserted -> PK violation on inventory.inbox).
        // grant itself has no reachable failure mode on this schema, so the failure is injected as an
        // adjacent statement in the shared transaction — which is precisely the coupling under test:
        // any doom of that transaction must roll back BOTH the inbox row and the grant.
        assertThrows(Exception::class.java) {
            QuarkusTransaction.requiringNew().call {
                inventory.onCharacterCreated(event)
                em.flush()
                em.createNativeQuery("INSERT INTO inventory.inbox(event_id) VALUES (?1)")
                    .setParameter(1, key)
                    .executeUpdate()
            }
        }

        assertEquals(0, inboxRows(key), "the rolled-back transaction must leave NO inbox row")
        assertTrue(inventory.holdings(owner).isEmpty(), "the grant rolled back with the inbox row")

        // Because the inbox row was NOT left behind, the redelivery is treated as first-seen and grants.
        inventory.onCharacterCreated(event)
        assertEquals(listOf("starter_sword" to 1), inventory.holdings(owner), "the retry reprocesses the grant")
        assertEquals(1, inboxRows(key), "the successful retry records exactly one inbox row")
    }

    private fun seedHolding(charId: Long, item: String, qty: Int) {
        db.connection.use { c ->
            c.prepareStatement(
                "INSERT INTO inventory.holdings(owner_type, owner_id, item, qty) VALUES ('CHARACTER', ?, ?, ?)",
            ).use { ps ->
                ps.setString(1, charId.toString())
                ps.setString(2, item)
                ps.setInt(3, qty)
                ps.executeUpdate()
            }
        }
    }

    private fun inboxRows(key: String): Int =
        db.connection.use { c ->
            c.prepareStatement("SELECT count(*) FROM inventory.inbox WHERE event_id = ?").use { ps ->
                ps.setString(1, key)
                ps.executeQuery().use { rs -> rs.next(); rs.getInt(1) }
            }
        }
}
