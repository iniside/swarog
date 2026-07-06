package domain

import characters.charactersevents.CharacterCreated
import com.fasterxml.jackson.databind.ObjectMapper
import io.quarkus.test.junit.QuarkusTest
import jakarta.inject.Inject
import java.util.UUID
import javax.sql.DataSource
import kotlin.random.Random
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

/**
 * P0.5 — locks THIS session's outbox-cleanup fix. Postgres `jsonb` canonicalizes on store (adds a
 * SPACE after every `:` and may reorder keys), so the OLD substring predicate
 * `payload::text LIKE '%"characterId":<id>%'` (no space) silently matches NOTHING, whereas the FIX
 * `payload->>'characterId' = '<id>'` extracts the value regardless of formatting. This inserts a REAL
 * serialized [CharacterCreated] row and proves the fix matches (1) while the old LIKE does not (0).
 *
 * The row uses a topic with no configured subscriber, so the characters outbox relay marks it sent and
 * fans out to nobody — no starter-item side effects. The row is removed in [cleanup] (delta-zero).
 */
@QuarkusTest
class OutboxJsonbPredicateTest {

    @Inject
    lateinit var db: DataSource

    @Inject
    lateinit var objectMapper: ObjectMapper

    private val characterId = 900_000_000_000L + Random.nextLong(0, 90_000_000_000L)

    @AfterEach
    fun cleanup() {
        db.connection.use { c ->
            c.prepareStatement("DELETE FROM characters.outbox WHERE payload->>'characterId' = ?").use { ps ->
                ps.setString(1, characterId.toString())
                ps.executeUpdate()
            }
        }
    }

    @Test
    fun `the extract predicate matches the jsonb row while the old LIKE predicate does not`() {
        val payload = objectMapper.writeValueAsString(CharacterCreated(characterId, UUID.randomUUID(), "JsonbProbe"))
        db.connection.use { c ->
            // Unknown topic => the relay marks it sent without any HTTP fan-out (no side effects).
            c.prepareStatement("INSERT INTO characters.outbox(topic, payload) VALUES ('characters.jsonbprobe', cast(? as jsonb))").use { ps ->
                ps.setString(1, payload)
                ps.executeUpdate()
            }
        }

        val byExtract = count("payload->>'characterId' = '$characterId'")
        val byOldLike = count("payload::text LIKE '%\"characterId\":$characterId%'")

        assertEquals(1, byExtract, "the FIX (->>) must match the stored jsonb row")
        assertEquals(0, byOldLike, "the OLD LIKE must miss it — jsonb inserts a space after the colon")
    }

    private fun count(predicate: String): Int =
        db.connection.use { c ->
            c.createStatement().use { s ->
                s.executeQuery("SELECT count(*) FROM characters.outbox WHERE $predicate").use { rs ->
                    rs.next(); rs.getInt(1)
                }
            }
        }
}
