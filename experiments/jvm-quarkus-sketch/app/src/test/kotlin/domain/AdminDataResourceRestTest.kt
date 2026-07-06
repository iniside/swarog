package domain

import characters.CharactersModule
import inventory.InventoryModule
import inventory.Owner
import inventory.OwnerType
import io.quarkus.test.junit.QuarkusTest
import io.quarkus.test.junit.TestProfile
import io.restassured.RestAssured.given
import jakarta.inject.Inject
import java.util.UUID
import javax.sql.DataSource
import org.hamcrest.Matchers.contains
import org.hamcrest.Matchers.equalTo
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

/**
 * P3-REST: the wire endpoints `GET /admin-data/characters` and `GET /admin-data/inventory` that a REMOTE
 * admin process fetches. Asserts the JSON matches the [admin.adminapi.AdminItemDto] shape (id/section/
 * label/data with kpis + table) AND that the KPI is behavioral — a DELTA measured around this test's own
 * seeded rows, never a global count (the shared `jvmsketch` DB is cumulative).
 *
 * Scheduler off ([SchedulerDisabledProfile]) so seeded characters don't fan out to inventory grants;
 * every seeded row (characters + player-owned holdings) is scoped to ids this test invents and removed in
 * [cleanup], leaving the DB delta-zero.
 */
@QuarkusTest
@TestProfile(SchedulerDisabledProfile::class)
class AdminDataResourceRestTest {

    @Inject
    lateinit var characters: CharactersModule

    @Inject
    lateinit var inventory: InventoryModule

    @Inject
    lateinit var db: DataSource

    private val characterIds = mutableListOf<Long>()
    private val ownerIds = mutableListOf<String>()

    @AfterEach
    fun cleanup() {
        db.connection.use { c ->
            for (id in characterIds) {
                c.prepareStatement("DELETE FROM characters.characters WHERE id = ?").use { ps ->
                    ps.setLong(1, id); ps.executeUpdate()
                }
                c.prepareStatement("DELETE FROM characters.outbox WHERE payload->>'characterId' = ?").use { ps ->
                    ps.setString(1, id.toString()); ps.executeUpdate()
                }
            }
            for (ownerId in ownerIds) {
                c.prepareStatement("DELETE FROM inventory.holdings WHERE owner_type = 'PLAYER' AND owner_id = ?").use { ps ->
                    ps.setString(1, ownerId); ps.executeUpdate()
                }
            }
        }
    }

    @Test
    fun `GET admin-data characters returns the AdminItemDto shape with a live delta KPI`() {
        val before = charactersKpiValue()

        val playerId = UUID.randomUUID()
        characterIds += characters.create(playerId, "AdminRestA")
        characterIds += characters.create(playerId, "AdminRestB")

        given()
            .`when`().get("/admin-data/characters")
            .then()
            .statusCode(200)
            .body("id", equalTo("characters"))
            .body("section", equalTo("Game Content"))
            .body("label", equalTo("Characters"))
            .body("data.kpis[0].label", equalTo("Characters"))
            .body("data.table.headers", contains("ID", "Player", "Name"))

        assertEquals(2, charactersKpiValue() - before, "the Characters KPI must reflect the two seeded rows")
    }

    @Test
    fun `GET admin-data inventory returns the AdminItemDto shape with a live delta KPI`() {
        val before = inventoryHoldingsKpiValue()

        // Two DISTINCT items for one player owner: Holdings +2 (authz-skip path for PLAYER owners).
        val owner = Owner(OwnerType.PLAYER, "admin-rest-player-${UUID.randomUUID()}")
        ownerIds += owner.id
        inventory.add(owner, "potion", 1)
        inventory.add(owner, "ether", 1)

        given()
            .`when`().get("/admin-data/inventory")
            .then()
            .statusCode(200)
            .body("id", equalTo("inventory"))
            .body("section", equalTo("Game Content"))
            .body("label", equalTo("Inventory"))
            .body("data.kpis[0].label", equalTo("Holdings"))
            .body("data.kpis[1].label", equalTo("Owners"))
            .body("data.table.headers", contains("Owner", "ID", "Item", "Qty"))

        assertEquals(2, inventoryHoldingsKpiValue() - before, "the Holdings KPI must reflect the two seeded rows")
    }

    private fun charactersKpiValue(): Int =
        given().`when`().get("/admin-data/characters")
            .then().statusCode(200)
            .extract().path<String>("data.kpis[0].value").toInt()

    private fun inventoryHoldingsKpiValue(): Int =
        given().`when`().get("/admin-data/inventory")
            .then().statusCode(200)
            .extract().path<String>("data.kpis[0].value").toInt()
}
