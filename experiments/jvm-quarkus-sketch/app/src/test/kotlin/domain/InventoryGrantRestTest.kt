package domain

import characters.charactersapi.CharactersUnavailableException
import characters.charactersapi.PlayerCharacters
import inventory.InventoryModule
import inventory.Owner
import inventory.OwnerType
import io.quarkus.test.InjectMock
import io.quarkus.test.junit.QuarkusTest
import io.restassured.RestAssured.given
import jakarta.inject.Inject
import java.util.UUID
import javax.sql.DataSource
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test
import org.mockito.Mockito

/**
 * P0-GRANT-REST: the `POST /inventory/{characterId}/grant` HTTP mapping in [inventory.InventoryResource].
 * The characters capability is replaced with a Mockito mock (proven substitutable in the Step-3a smoke:
 * `@InjectMock` DOES override the `@Produces @ApplicationScoped PlayerCharacters` producer bean), so
 * each authorization outcome is driven precisely without a live QUIC upstream:
 *  - owned character            → 200 + the holding is persisted (incl. the `item ?: "starter_sword"`,
 *                                 `qty ?: 1` defaults),
 *  - unknown character (`-1`)   → the module's `error(...)` → IllegalStateException → 400,
 *  - upstream unreachable        → [CharactersUnavailableException] → 503 (NOT conflated with the 400).
 *
 * Only the two success cases write a `holdings` row; both use large synthetic character ids (mocked
 * `ownerOf`, so no real character row is needed) and are deleted in [cleanup], leaving the shared DB
 * delta-zero.
 */
@QuarkusTest
class InventoryGrantRestTest {

    @InjectMock
    lateinit var players: PlayerCharacters

    @Inject
    lateinit var inventory: InventoryModule

    @Inject
    lateinit var db: DataSource

    private val ownedCharacter = 700_000_101L
    private val defaultsCharacter = 700_000_102L

    @AfterEach
    fun cleanup() {
        db.connection.use { c ->
            c.prepareStatement("DELETE FROM inventory.holdings WHERE owner_type = 'CHARACTER' AND owner_id = ANY(?)").use { ps ->
                ps.setArray(1, c.createArrayOf("text", arrayOf(ownedCharacter.toString(), defaultsCharacter.toString())))
                ps.executeUpdate()
            }
        }
    }

    @Test
    fun `grant on an owned character returns 200 and persists the holding`() {
        Mockito.`when`(players.ownerOf(Mockito.anyLong())).thenReturn(UUID.randomUUID())

        given()
            .queryParam("item", "shield")
            .queryParam("qty", 3)
            .`when`().post("/inventory/$ownedCharacter/grant")
            .then().statusCode(200)

        assertEquals(
            listOf("shield" to 3),
            inventory.holdings(Owner(OwnerType.CHARACTER, ownedCharacter.toString())),
        )
    }

    @Test
    fun `grant without item or qty applies the starter_sword x1 defaults`() {
        Mockito.`when`(players.ownerOf(Mockito.anyLong())).thenReturn(UUID.randomUUID())

        given()
            .`when`().post("/inventory/$defaultsCharacter/grant")
            .then().statusCode(200)

        assertEquals(
            listOf("starter_sword" to 1),
            inventory.holdings(Owner(OwnerType.CHARACTER, defaultsCharacter.toString())),
        )
    }

    @Test
    fun `grant on an unknown character returns 400, not 503`() {
        // mock ownerOf defaults to null (unstubbed) => the module rejects with error(...) => 400.
        given()
            .`when`().post("/inventory/-1/grant")
            .then().statusCode(400)
    }

    @Test
    fun `grant when the characters upstream is unreachable returns 503, not 400`() {
        Mockito.`when`(players.ownerOf(Mockito.anyLong()))
            .thenThrow(CharactersUnavailableException("upstream down"))

        given()
            .`when`().post("/inventory/$ownedCharacter/grant")
            .then().statusCode(503)
    }
}
