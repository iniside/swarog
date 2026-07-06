package domain

import admin.adminapi.AdminItemDto
import characters.CharactersAdminData
import characters.CharactersModule
import io.quarkus.narayana.jta.QuarkusTransaction
import io.quarkus.test.junit.QuarkusTest
import io.quarkus.test.junit.TestProfile
import io.restassured.RestAssured.given
import jakarta.inject.Inject
import java.util.UUID
import javax.sql.DataSource
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

/**
 * P3-ADMIN-PARITY: a module must render the SAME [AdminItemDto] whether the admin serves it LOCALLY
 * (the in-process [admin.adminapi.AdminDataProvider] bean it discovers via `@All`) or REMOTELY (over the
 * `/admin-data/<id>` REST fan-out). We seed rows, then compare the local `provider.data()` output against
 * what `GET /admin-data/characters` returns for the SAME DB state: id/section/label must agree, and the
 * KPI + table structure (labels, values, headers, row count) must agree.
 *
 * LIMIT (honest): a TRUE two-topology comparison — the module co-located in the admin process vs. living
 * in a SECOND JVM reached over the network — needs the `install.ps1 -Mode microservices` process split,
 * which is out of scope in-JVM. This is the achievable in-process version: the local provider bean vs its
 * OWN `/admin-data` endpoint, both backed by the same DB. It proves the two code paths that build the DTO
 * (local field read vs REST serialize→deserialize) agree; it does NOT exercise a real network hop. The
 * scheduler-off boot keeps DB state stable between the two reads so the value comparison is deterministic.
 */
@QuarkusTest
@TestProfile(SchedulerDisabledProfile::class)
class AdminParityTest {

    @Inject
    lateinit var provider: CharactersAdminData

    @Inject
    lateinit var characters: CharactersModule

    @Inject
    lateinit var db: DataSource

    private val cleanupIds = mutableListOf<Long>()

    @AfterEach
    fun cleanup() {
        db.connection.use { c ->
            for (id in cleanupIds) {
                c.prepareStatement("DELETE FROM characters.characters WHERE id = ?").use { ps ->
                    ps.setLong(1, id); ps.executeUpdate()
                }
                c.prepareStatement("DELETE FROM characters.outbox WHERE payload->>'characterId' = ?").use { ps ->
                    ps.setString(1, id.toString()); ps.executeUpdate()
                }
            }
        }
    }

    @Test
    fun `characters renders the same AdminItemDto locally and over the REST fan-out`() {
        val playerId = UUID.randomUUID()
        cleanupIds += characters.create(playerId, "ParityA")
        cleanupIds += characters.create(playerId, "ParityB")

        // LOCAL path: exactly what AdminResource builds from the co-located provider bean.
        val local = QuarkusTransaction.requiringNew().call {
            AdminItemDto(provider.id, provider.section, provider.label, provider.data())
        }

        // REMOTE path: the wire endpoint, read field-by-field (avoids depending on a Kotlin-aware
        // deserializer on the RestAssured classpath).
        val remote = given().`when`().get("/admin-data/characters").then().statusCode(200).extract()

        assertEquals(local.id, remote.path("id"), "id must match")
        assertEquals(local.section, remote.path("section"), "section must match")
        assertEquals(local.label, remote.path("label"), "label must match")
        assertEquals(local.data.kpis.map { it.label }, remote.path("data.kpis.label"), "KPI labels must match")
        assertEquals(local.data.kpis.map { it.value }, remote.path("data.kpis.value"), "KPI values must match")
        assertEquals(local.data.table?.headers, remote.path("data.table.headers"), "table headers must match")
        assertEquals(local.data.table?.rows?.size, remote.path<List<Any>>("data.table.rows").size, "table row count must match")
    }
}
