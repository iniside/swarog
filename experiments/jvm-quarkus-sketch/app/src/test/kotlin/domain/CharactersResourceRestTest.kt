package domain

import io.quarkus.test.junit.QuarkusTest
import io.quarkus.test.junit.TestProfile
import io.restassured.RestAssured.given
import jakarta.inject.Inject
import java.sql.ResultSet
import java.util.UUID
import javax.sql.DataSource
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

/**
 * P3-REST: the [characters.CharactersResource] HTTP surface — the DRIVER endpoint that create/deletes
 * characters over HTTP (the only way to drive the create → outbox fanout in split mode). Exercises the
 * REST mapping end to end:
 *  - `POST /characters?name&playerId` → 200 + a `characters.created` outbox row for the returned id,
 *  - `POST /characters` with no name → defaults the name to "unnamed",
 *  - `DELETE /characters/{id}` (existing) → 204 + a `characters.deleted` outbox row,
 *  - `DELETE /characters/{id}` (nonexistent) → 204 (silent no-op over HTTP, no deleted row).
 *
 * The scheduler is off ([SchedulerDisabledProfile]) so create/delete write only their `characters`-schema
 * rows without the relay fanning out to inventory. Every created id is deleted in [cleanup] (characters
 * row + all its outbox rows), leaving the shared DB delta-zero. The malformed-playerId → 400 case is
 * already covered in [CharactersBehaviorTest] (4a) and is not duplicated here.
 */
@QuarkusTest
@TestProfile(SchedulerDisabledProfile::class)
class CharactersResourceRestTest {

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
    fun `POST characters creates a character and writes a characters-created outbox row`() {
        val playerId = UUID.randomUUID()

        val id = given()
            .queryParam("name", "RestHero")
            .queryParam("playerId", playerId.toString())
            .`when`().post("/characters")
            .then().statusCode(200)
            .extract().asString().trim().toLong()
        cleanupIds += id

        assertEquals(1, outboxRowsFor(id, "characters.created"), "POST must append exactly one created outbox row")
    }

    @Test
    fun `POST characters with no name defaults the character name to unnamed`() {
        val id = given()
            .queryParam("playerId", UUID.randomUUID().toString())
            .`when`().post("/characters")
            .then().statusCode(200)
            .extract().asString().trim().toLong()
        cleanupIds += id

        assertEquals("unnamed", characterName(id), "a missing name must default to 'unnamed'")
    }

    @Test
    fun `DELETE characters of an existing id returns 204 and writes a characters-deleted outbox row`() {
        val id = given()
            .queryParam("name", "ToDelete")
            .queryParam("playerId", UUID.randomUUID().toString())
            .`when`().post("/characters")
            .then().statusCode(200)
            .extract().asString().trim().toLong()
        cleanupIds += id

        given().`when`().delete("/characters/$id").then().statusCode(204)

        assertEquals(1, outboxRowsFor(id, "characters.deleted"), "DELETE of an existing id must append a deleted row")
    }

    @Test
    fun `DELETE characters of a nonexistent id returns 204 and writes no outbox row`() {
        val ghost = Long.MAX_VALUE - 54_321L
        cleanupIds += ghost   // defensive: if the no-op guard regresses, don't leak a row across runs

        given().`when`().delete("/characters/$ghost").then().statusCode(204)

        assertEquals(0, outboxRowsFor(ghost, "characters.deleted"), "a no-op delete must not append any outbox row")
    }

    private fun outboxRowsFor(characterId: Long, topic: String): Int =
        db.connection.use { c ->
            c.prepareStatement(
                "SELECT count(*) FROM characters.outbox WHERE topic = ? AND payload->>'characterId' = ?",
            ).use { ps ->
                ps.setString(1, topic)
                ps.setString(2, characterId.toString())
                ps.executeQuery().use { rs -> rs.next(); rs.getInt(1) }
            }
        }

    private fun characterName(id: Long): String? =
        db.connection.use { c ->
            c.prepareStatement("SELECT name FROM characters.characters WHERE id = ?").use { ps ->
                ps.setLong(1, id)
                ps.executeQuery().use(::firstString)
            }
        }

    // Extracted so the JDBC try-with-resources chain doesn't nest the branch too deep (detekt).
    private fun firstString(rs: ResultSet): String? = if (rs.next()) rs.getString(1) else null
}
