package domain

import accounts.AccountsModule
import accounts.AccountsOutboxRelay
import accounts.accountsevents.PlayerRegistered
import com.fasterxml.jackson.databind.ObjectMapper
import com.sun.net.httpserver.HttpServer
import io.quarkus.narayana.jta.QuarkusTransaction
import io.quarkus.test.junit.QuarkusTest
import io.quarkus.test.junit.TestProfile
import jakarta.inject.Inject
import jakarta.persistence.EntityManager
import java.net.InetSocketAddress
import java.sql.ResultSet
import java.sql.Timestamp
import java.util.Optional
import java.util.UUID
import java.util.concurrent.atomic.AtomicInteger
import javax.sql.DataSource
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Test
import platform.RoleConfig

/**
 * P1-ACCOUNTS behavioral tests against the local `jvmsketch` Postgres (no Docker). The scheduler is
 * disabled ([SchedulerDisabledProfile]) so:
 *  - the `drain()` tests are driven MANUALLY (a hand-constructed relay pointed at a stub URL), with no
 *    background relay racing to mark the same row sent, and
 *  - `register()`'s committed rows are not asynchronously drained/HTTP-fanned before assertions.
 *
 * Every row is id/marker-scoped and removed in [cleanup], leaving the shared DB delta-zero. No global
 * counts (the DB is cumulative across boots).
 */
@QuarkusTest
@TestProfile(SchedulerDisabledProfile::class)
class AccountsBehaviorTest {

    @Inject
    lateinit var accounts: AccountsModule

    @Inject
    lateinit var db: DataSource

    @Inject
    lateinit var em: EntityManager

    @Inject
    lateinit var objectMapper: ObjectMapper

    @Inject
    lateinit var roleConfig: RoleConfig

    private val cleanupIds = mutableListOf<UUID>()

    @AfterEach
    fun cleanup() {
        db.connection.use { c ->
            for (id in cleanupIds) {
                c.prepareStatement("DELETE FROM accounts.outbox WHERE payload->>'playerId' = ?").use { ps ->
                    ps.setString(1, id.toString())
                    ps.executeUpdate()
                }
                c.prepareStatement("DELETE FROM accounts.players WHERE id = ?").use { ps ->
                    ps.setObject(1, id)
                    ps.executeUpdate()
                }
            }
        }
    }

    @Test
    fun `register writes exactly one player and one outbox row, atomically`() {
        val id = accounts.register("p1-happy")
        cleanupIds += id

        assertEquals(1, playerRows(id), "exactly one accounts.players row for the registered id")
        assertEquals(1, outboxRows(id), "exactly one accounts.outbox row for the registered id")
    }

    @Test
    fun `a failure mid-transaction rolls back BOTH the player and the outbox row`() {
        var probeId: UUID? = null

        // Enclose register() in a transaction it joins (@Transactional REQUIRED), then force a PK
        // violation in that SAME transaction. If player + outbox were not atomic, one could survive.
        assertThrows(Exception::class.java) {
            QuarkusTransaction.requiringNew().call {
                val id = accounts.register("p1-rollback")
                probeId = id
                em.flush()   // materialize register's INSERTs inside the tx
                // Duplicate PRIMARY KEY on accounts.players -> constraint violation -> whole tx doomed.
                em.createNativeQuery("INSERT INTO accounts.players(id, provider) VALUES (?1, ?2)")
                    .setParameter(1, id)
                    .setParameter(2, "duplicate")
                    .executeUpdate()
            }
        }

        val id = checkNotNull(probeId) { "register must have produced an id before the forced failure" }
        cleanupIds += id   // nothing committed, but stay defensive
        assertEquals(0, playerRows(id), "rollback must leave NO accounts.players row")
        assertEquals(0, outboxRows(id), "rollback must leave NO accounts.outbox row")
    }

    @Test
    fun `zero-subscriber drain marks the row sent immediately with no HTTP`() {
        val marker = UUID.randomUUID()
        cleanupIds += marker
        insertRegisteredOutbox(marker)

        // No subscriber configured for accounts.registered -> subscribers is empty -> the relay marks
        // the row sent WITHOUT ever entering the HTTP path (postToAll is unreachable when empty).
        val relay = AccountsOutboxRelay(db, roleConfig, Optional.empty())
        relay.drain()

        assertNotNull(sentAt(marker), "a zero-subscriber row must be marked sent on the first drain")
    }

    @Test
    fun `a non-2xx subscriber leaves the row unsent and is retried on the next drain`() {
        val hits = AtomicInteger(0)
        val server = HttpServer.create(InetSocketAddress("127.0.0.1", 0), 0)
        server.createContext("/") { exchange ->
            hits.incrementAndGet()
            exchange.sendResponseHeaders(500, -1)   // -1 = no response body
            exchange.close()
        }
        server.start()
        try {
            val url = "http://127.0.0.1:${server.address.port}/"
            val marker = UUID.randomUUID()
            cleanupIds += marker
            insertRegisteredOutbox(marker)

            val relay = AccountsOutboxRelay(db, roleConfig, Optional.of(url))

            relay.drain()
            assertEquals(1, hits.get(), "first drain POSTs once")
            assertNull(sentAt(marker), "a 500 response must leave the row unsent")

            relay.drain()
            assertEquals(2, hits.get(), "the still-unsent row is re-POSTed on the next drain")
            assertNull(sentAt(marker), "still 500 -> still unsent after the retry")
        } finally {
            server.stop(0)
        }
    }

    private fun insertRegisteredOutbox(marker: UUID) {
        val payload = objectMapper.writeValueAsString(PlayerRegistered(marker, "relay-test"))
        db.connection.use { c ->
            c.prepareStatement(
                "INSERT INTO accounts.outbox(topic, payload) VALUES (?, cast(? as jsonb))",
            ).use { ps ->
                ps.setString(1, PlayerRegistered.TOPIC)
                ps.setString(2, payload)
                ps.executeUpdate()
            }
        }
    }

    private fun playerRows(id: UUID): Int =
        db.connection.use { c ->
            c.prepareStatement("SELECT count(*) FROM accounts.players WHERE id = ?").use { ps ->
                ps.setObject(1, id)
                ps.executeQuery().use { rs -> rs.next(); rs.getInt(1) }
            }
        }

    private fun outboxRows(id: UUID): Int =
        db.connection.use { c ->
            c.prepareStatement("SELECT count(*) FROM accounts.outbox WHERE payload->>'playerId' = ?").use { ps ->
                ps.setString(1, id.toString())
                ps.executeQuery().use { rs -> rs.next(); rs.getInt(1) }
            }
        }

    private fun sentAt(marker: UUID): Timestamp? =
        db.connection.use { c ->
            c.prepareStatement("SELECT sent_at FROM accounts.outbox WHERE payload->>'playerId' = ?").use { ps ->
                ps.setString(1, marker.toString())
                ps.executeQuery().use(::firstSentAt)
            }
        }

    // Extracted so the JDBC try-with-resources chain doesn't nest the branch too deep (detekt).
    private fun firstSentAt(rs: ResultSet): Timestamp? = if (rs.next()) rs.getTimestamp("sent_at") else null
}
