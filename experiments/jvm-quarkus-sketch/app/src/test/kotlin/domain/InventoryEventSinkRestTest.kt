package domain

import inventory.InventoryModule
import inventory.Owner
import inventory.OwnerType
import io.quarkus.narayana.jta.QuarkusTransaction
import io.quarkus.test.junit.QuarkusTest
import io.quarkus.test.junit.TestProfile
import io.restassured.RestAssured.given
import io.restassured.http.ContentType
import jakarta.inject.Inject
import java.util.UUID
import javax.sql.DataSource
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * P3-REST: the [inventory.InventoryEventSink] HTTP surface — the broker-less subscriber the characters
 * relay POSTs to. This exercises the REAL endpoint (JSON body → the `@Consumes` deserializes a
 * [characters.charactersevents.CharacterCreated]/`CharacterDeleted`) rather than calling the module
 * handler directly, so it proves the sink correctly maps a delivered event to its side effect:
 *  - `POST /events/character-created` → 200 and the starter grant lands (id-scoped),
 *  - `POST /events/character-deleted` → 200 and the character's holdings are wiped.
 *
 * The scheduler is off ([SchedulerDisabledProfile]) so the only writes are this test's direct POSTs —
 * no outbox relay races in. Every row (holdings + the idempotency inbox marker) is scoped to synthetic
 * character ids this test invents and removed in [cleanup], leaving the shared DB delta-zero. The FAULT
 * path (a handler that throws must NOT be swallowed to 200) lives in [InventoryEventSinkFaultTest],
 * which needs a mocked module and thus its own boot.
 */
@QuarkusTest
@TestProfile(SchedulerDisabledProfile::class)
class InventoryEventSinkRestTest {

    @Inject
    lateinit var inventory: InventoryModule

    @Inject
    lateinit var db: DataSource

    private val createdCharacter = 810_000_301L
    private val deletedCharacter = 810_000_302L

    @AfterEach
    fun cleanup() {
        val ids = listOf(createdCharacter, deletedCharacter)
        db.connection.use { c ->
            c.prepareStatement("DELETE FROM inventory.holdings WHERE owner_type = 'CHARACTER' AND owner_id = ANY(?)").use { ps ->
                ps.setArray(1, c.createArrayOf("text", ids.map { it.toString() }.toTypedArray()))
                ps.executeUpdate()
            }
            // Both onCharacterCreated and onCharacterDeleted record an inbox marker "<topic>:<id>".
            c.prepareStatement("DELETE FROM inventory.inbox WHERE event_id = ANY(?)").use { ps ->
                val markers = ids.flatMap { listOf("characters.created:$it", "characters.deleted:$it") }
                ps.setArray(1, c.createArrayOf("text", markers.toTypedArray()))
                ps.executeUpdate()
            }
        }
    }

    @Test
    fun `POST character-created returns 200 and the starter grant lands`() {
        given()
            .contentType(ContentType.JSON)
            .body(createdBody(createdCharacter))
            .`when`().post("/events/character-created")
            .then().statusCode(200)

        assertEquals(
            listOf("starter_sword" to 1),
            holdingsOf(createdCharacter),
            "the sink must apply onCharacterCreated's starter grant",
        )
    }

    @Test
    fun `POST character-deleted returns 200 and the character's holdings are wiped`() {
        // Seed a holding by delivering a create first (its own inbox marker, no dedup clash with delete).
        given()
            .contentType(ContentType.JSON)
            .body(createdBody(deletedCharacter))
            .`when`().post("/events/character-created")
            .then().statusCode(200)
        assertTrue(holdingsOf(deletedCharacter).isNotEmpty(), "precondition: the character has a holding")

        given()
            .contentType(ContentType.JSON)
            .body(deletedBody(deletedCharacter))
            .`when`().post("/events/character-deleted")
            .then().statusCode(200)

        assertTrue(holdingsOf(deletedCharacter).isEmpty(), "the sink must wipe the deleted character's holdings")
    }

    private fun holdingsOf(characterId: Long): List<Pair<String, Int>> =
        QuarkusTransaction.requiringNew().call {
            inventory.holdings(Owner(OwnerType.CHARACTER, characterId.toString()))
        }

    private fun createdBody(characterId: Long): String =
        """{"characterId":$characterId,"playerId":"${UUID.randomUUID()}","name":"SinkHero"}"""

    private fun deletedBody(characterId: Long): String =
        """{"characterId":$characterId,"playerId":"${UUID.randomUUID()}"}"""
}
