package domain

import characters.CharactersModule
import characters.charactersevents.CharacterCreated
import inventory.InventoryModule
import inventory.Owner
import inventory.OwnerType
import io.quarkus.test.junit.QuarkusTest
import jakarta.inject.Inject
import java.util.UUID
import javax.sql.DataSource
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Test

/**
 * Two invariants of [InventoryModule] exercised directly against the container-wired bean (real
 * Postgres, real [characters.charactersapi.PlayerCharacters] producer) rather than through the HTTP
 * event pipeline covered by [InventoryCharacterLifecycleEventTest]:
 *  - the inbox dedup: a "redelivered" event (the outbox relay's at-least-once retry) must not double
 *    the granted quantity — simulated here by invoking the handler an extra time with the SAME event
 *    the real relay already delivered once, since forcing an actual HTTP redelivery would require
 *    making the first POST fail on purpose.
 *  - the authorization seam: `add()` for a CHARACTER owner only succeeds for a character that is
 *    actually owned; an unknown id is rejected before any write.
 *
 * Every test that creates a character via [CharactersModule.create] first waits for that character's
 * OWN async starter-item grant to land (the same outbox -> HTTP -> inbox pipeline as
 * [InventoryCharacterLifecycleEventTest]) before doing anything else — otherwise the grant could land
 * AFTER the test body finishes and AFTER [cleanup] already ran, leaking an orphan holding into later
 * tests. This was caught empirically: an earlier version of this test that skipped the wait leaked
 * exactly such orphans (see the plan's deliberate-break notes for the reverse case).
 */
@QuarkusTest
class InventoryAuthorizationAndDedupTest {

    @Inject
    lateinit var characters: CharactersModule

    @Inject
    lateinit var inventory: InventoryModule

    @Inject
    lateinit var db: DataSource

    private var characterId: Long? = null

    @AfterEach
    fun cleanup() {
        val id = characterId ?: return
        // Defensive: every test already waits for its OWN created-event grant to land before
        // finishing, but this is a cheap extra guard against any outbox row for this character still
        // being retried in the background before we delete its rows.
        awaitOutboxDrained(id)
        db.connection.use { c ->
            c.prepareStatement("DELETE FROM characters.characters WHERE id = ?").use { ps ->
                ps.setLong(1, id)
                ps.executeUpdate()
            }
            c.prepareStatement("DELETE FROM characters.outbox WHERE payload->>'characterId' = ?").use { ps ->
                ps.setString(1, id.toString())
                ps.executeUpdate()
            }
            c.prepareStatement("DELETE FROM inventory.holdings WHERE owner_type = 'CHARACTER' AND owner_id = ?").use { ps ->
                ps.setString(1, id.toString())
                ps.executeUpdate()
            }
            c.prepareStatement("DELETE FROM inventory.inbox WHERE event_id LIKE ?").use { ps ->
                ps.setString(1, "%:$id")
                ps.executeUpdate()
            }
        }
    }

    private fun awaitOutboxDrained(id: Long, timeoutMs: Long = 3000) {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < deadline && unsentOutboxRows(id) > 0) {
            Thread.sleep(25)
        }
    }

    private fun unsentOutboxRows(id: Long): Int =
        db.connection.use { c ->
            c.prepareStatement(
                "SELECT count(*) FROM characters.outbox WHERE payload->>'characterId' = ? AND sent_at IS NULL",
            ).use { ps ->
                ps.setString(1, id.toString())
                ps.executeQuery().use { rs -> rs.next(); rs.getInt(1) }
            }
        }

    /** Creates a character and waits for ITS OWN created-event to land (starter_sword granted)
     *  before returning, so the caller never races the async outbox -> HTTP -> inbox pipeline. */
    private fun createAndAwaitStarterGrant(name: String): Pair<Long, Owner> {
        val playerId = UUID.randomUUID()
        val id = characters.create(playerId, name)
        characterId = id
        val owner = Owner(OwnerType.CHARACTER, id.toString())
        val deadline = System.currentTimeMillis() + 5000
        while (inventory.holdings(owner) != listOf("starter_sword" to 1)) {
            check(System.currentTimeMillis() <= deadline) { "timed out waiting for $name's starter item" }
            Thread.sleep(25)
        }
        return id to owner
    }

    @Test
    fun `redelivered character-created event grants the starter item only once`() {
        val (id, owner) = createAndAwaitStarterGrant("DedupHero")

        // Simulated redelivery: the relay's at-least-once retry re-POSTs the SAME already-delivered
        // event; the inbox dedup (keyed by topic+characterId) must make this a no-op.
        inventory.onCharacterCreated(CharacterCreated(id, UUID.randomUUID(), "DedupHero"))

        assertEquals(listOf("starter_sword" to 1), inventory.holdings(owner))
    }

    @Test
    fun `add rejects an unowned character id`() {
        assertThrows(IllegalStateException::class.java) {
            inventory.add(Owner(OwnerType.CHARACTER, "-1"), "sword", 1)
        }
    }

    @Test
    fun `add persists a holding for a real owned character`() {
        val (_, owner) = createAndAwaitStarterGrant("OwnedHero")

        inventory.add(owner, "healing_potion", 2)

        assertEquals(listOf("healing_potion" to 2, "starter_sword" to 1), inventory.holdings(owner))
    }
}
