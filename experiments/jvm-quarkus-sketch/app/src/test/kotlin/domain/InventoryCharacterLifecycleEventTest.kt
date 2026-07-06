package domain

import characters.CharactersModule
import inventory.InventoryModule
import inventory.Owner
import inventory.OwnerType
import io.quarkus.test.junit.QuarkusTest
import jakarta.inject.Inject
import java.util.UUID
import javax.sql.DataSource
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * The crown-jewel proof (CLAUDE.md "the point"): cross-module integrity comes from an EVENT, not an
 * FK cascade. Unlike [InventoryAuthorizationAndDedupTest] (which calls [InventoryModule]'s handlers
 * directly), this test drives the REAL pipeline end to end: [CharactersModule.create]/`delete` append
 * to `characters.outbox`, [characters.CharactersOutboxRelay] POSTs each row to
 * `inventory.InventoryEventSink` over HTTP, and the inbox-deduped handler mutates `inventory.holdings`
 * — the same wiring production uses. `quarkus.http.test-port=8090` (src/test/resources) is what makes
 * the relay's default `localhost:8090` target land on THIS test's own server.
 *
 * DB strategy: @QuarkusTest against the local `jvmsketch` Postgres (no Docker on this box, so no Dev
 * Services) — each test cleans up the rows it created in [cleanup].
 */
@QuarkusTest
class InventoryCharacterLifecycleEventTest {

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
        // This test already awaits the create+delete events landing, but the relay could still be
        // mid-retry for either outbox row (e.g. a transient failure) when the test method returns.
        // Draining first avoids a late delivery inserting/removing rows AFTER cleanup runs.
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

    @Test
    fun `character created grants a starter item, character deleted wipes it - no orphans remain`() {
        val playerId = UUID.randomUUID()
        val id = characters.create(playerId, "IntegrationHero")
        characterId = id
        val owner = Owner(OwnerType.CHARACTER, id.toString())

        awaitTrue("starter item granted via outbox -> HTTP -> inbox") {
            inventory.holdings(owner) == listOf("starter_sword" to 1)
        }

        characters.delete(id)

        awaitTrue("deleted character's holdings wiped via outbox -> HTTP -> inbox (no orphan rows)") {
            inventory.holdings(owner).isEmpty()
        }
    }

    /** Same eventual-consistency polling style as `app.Seed.awaitUntil` — the bus/relay is async. */
    private fun awaitTrue(what: String, timeoutMs: Long = 5000, cond: () -> Boolean) {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (!cond()) {
            check(System.currentTimeMillis() <= deadline) { "timed out waiting for: $what" }
            Thread.sleep(25)
        }
    }
}
