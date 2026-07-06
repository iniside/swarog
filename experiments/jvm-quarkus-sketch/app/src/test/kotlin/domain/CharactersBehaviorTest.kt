package domain

import admin.adminapi.Cell
import characters.Character
import characters.CharactersAdminData
import characters.CharactersModule
import characters.LocalPlayerCharacters
import io.quarkus.narayana.jta.QuarkusTransaction
import io.quarkus.test.junit.QuarkusTest
import io.quarkus.test.junit.TestProfile
import io.restassured.RestAssured.given
import jakarta.inject.Inject
import java.sql.ResultSet
import java.util.UUID
import javax.sql.DataSource
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertDoesNotThrow
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * P1-CHARACTERS behavioral tests against the local `jvmsketch` Postgres. The scheduler is disabled
 * ([SchedulerDisabledProfile]) so `create()` writes its character + outbox rows WITHOUT the relay
 * fanning out to the inventory grant — the only rows this test produces are in the `characters` schema,
 * cleaned up by id in [cleanup] (no inventory side effects, DB delta-zero). Panache reads outside a
 * request run inside `QuarkusTransaction.requiringNew()` — the exact wrapper the edge server uses.
 */
@QuarkusTest
@TestProfile(SchedulerDisabledProfile::class)
class CharactersBehaviorTest {

    @Inject
    lateinit var characters: CharactersModule

    @Inject
    lateinit var local: LocalPlayerCharacters

    @Inject
    lateinit var adminData: CharactersAdminData

    @Inject
    lateinit var db: DataSource

    private val cleanupIds = mutableListOf<Long>()

    @AfterEach
    fun cleanup() {
        db.connection.use { c ->
            for (id in cleanupIds) {
                c.prepareStatement("DELETE FROM characters.characters WHERE id = ?").use { ps ->
                    ps.setLong(1, id)
                    ps.executeUpdate()
                }
                c.prepareStatement("DELETE FROM characters.outbox WHERE payload->>'characterId' = ?").use { ps ->
                    ps.setString(1, id.toString())
                    ps.executeUpdate()
                }
            }
        }
    }

    @Test
    fun `create flushes before it appends - the outbox characterId equals the returned id`() {
        val playerId = UUID.randomUUID()   // unique, so the created outbox row is findable by it

        val id = characters.create(playerId, "FlushProbe")
        cleanupIds += id

        // The id must have been assigned (flush) BEFORE it went into the payload: the created row's
        // payload characterId must equal the value create() returned.
        assertEquals(id.toString(), createdOutboxCharacterId(playerId))
    }

    @Test
    fun `delete of a nonexistent character is a silent no-op with no outbox row`() {
        val ghost = Long.MAX_VALUE - 12_345L
        cleanupIds += ghost   // defensive: if the no-op guard regresses, don't leak a row across runs

        assertDoesNotThrow { characters.delete(ghost) }
        assertEquals(0, outboxRowsFor(ghost), "a no-op delete must not append any outbox row")
    }

    @Test
    fun `ownerOf returns the owning player for a known id and null for an unknown id`() {
        val playerId = UUID.randomUUID()
        val id = characters.create(playerId, "OwnerProbe")
        cleanupIds += id

        val known = QuarkusTransaction.requiringNew().call { local.ownerOf(id) }
        assertEquals(playerId, known, "ownerOf(known) must return the owning player UUID")

        val unknown = QuarkusTransaction.requiringNew().call { local.ownerOf(Long.MAX_VALUE) }
        assertNull(unknown, "ownerOf(unknown) must return null, never throw")
    }

    @Test
    fun `admin data reports the live count and the most-recent-10 characters, id-descending`() {
        val playerId = UUID.randomUUID()
        val first = characters.create(playerId, "AdminA")
        val second = characters.create(playerId, "AdminB")
        val third = characters.create(playerId, "AdminC")
        cleanupIds += listOf(first, second, third)

        val data = QuarkusTransaction.requiringNew().call { adminData.data() }
        val liveCount = QuarkusTransaction.requiringNew().call { Character.count() }

        // KPI: label fixed, value is the LIVE total (behavioral — not a hardcoded global count).
        val kpi = data.kpis.single()
        assertEquals("Characters", kpi.label)
        assertEquals(liveCount.toString(), kpi.value)

        val table = checkNotNull(data.table) { "admin data must include a table" }
        assertEquals(listOf("ID", "Player", "Name"), table.headers)
        assertTrue(table.rows.size <= 10, "the table is capped at the 10 most recent rows")

        // Nothing is created after these three (scheduler off, tests sequential), so they are the newest
        // ids and occupy the top of the id-DESC page — newest first. Exact Cell shape is pinned too.
        val topThree = table.rows.take(3)
        assertEquals(
            listOf(Cell(third.toString(), mono = true), Cell(playerId.toString(), mono = true), Cell("AdminC")),
            topThree[0],
        )
        assertEquals(
            listOf(Cell(second.toString(), mono = true), Cell(playerId.toString(), mono = true), Cell("AdminB")),
            topThree[1],
        )
        assertEquals(
            listOf(Cell(first.toString(), mono = true), Cell(playerId.toString(), mono = true), Cell("AdminA")),
            topThree[2],
        )
    }

    @Test
    fun `POST characters with a malformed playerId returns 400, not 500`() {
        given()
            .queryParam("playerId", "not-a-uuid")
            .`when`().post("/characters")
            .then().statusCode(400)
    }

    private fun createdOutboxCharacterId(playerId: UUID): String? =
        db.connection.use { c ->
            c.prepareStatement(
                "SELECT payload->>'characterId' FROM characters.outbox " +
                    "WHERE topic = 'characters.created' AND payload->>'playerId' = ?",
            ).use { ps ->
                ps.setString(1, playerId.toString())
                ps.executeQuery().use(::firstString)
            }
        }

    // Extracted so the JDBC try-with-resources chain doesn't nest the branch too deep (detekt).
    private fun firstString(rs: ResultSet): String? = if (rs.next()) rs.getString(1) else null

    private fun outboxRowsFor(characterId: Long): Int =
        db.connection.use { c ->
            c.prepareStatement("SELECT count(*) FROM characters.outbox WHERE payload->>'characterId' = ?").use { ps ->
                ps.setString(1, characterId.toString())
                ps.executeQuery().use { rs -> rs.next(); rs.getInt(1) }
            }
        }
}
