package accounts

import accounts.accountsevents.PlayerRegistered
import com.fasterxml.jackson.databind.ObjectMapper
import io.quarkus.runtime.StartupEvent
import jakarta.enterprise.context.ApplicationScoped
import jakarta.enterprise.event.Observes
import jakarta.persistence.EntityManager
import jakarta.transaction.Transactional
import java.util.UUID
import javax.sql.DataSource
import platform.RoleConfig

/** Owns player identity. Foundation module — depends on nothing but the container. */
@ApplicationScoped
class AccountsModule(
    private val db: DataSource,
    private val em: EntityManager,
    private val objectMapper: ObjectMapper,
    private val roleConfig: RoleConfig,
) {

    /** Own schema only, raw DDL — migrations stay the module's property; the ORM maps the result.
     *  Default observer priority: order-independent by construction (no cross-module FKs). */
    fun migrate(@Observes ev: StartupEvent) {
        if (!roleConfig.isActive("accounts")) return
        db.connection.use { c ->
            c.createStatement().use { s ->
                s.execute("CREATE SCHEMA IF NOT EXISTS accounts")
                s.execute(
                    """CREATE TABLE IF NOT EXISTS accounts.players(
                        id         UUID PRIMARY KEY,
                        provider   TEXT NOT NULL,
                        created_at TIMESTAMPTZ NOT NULL DEFAULT now())"""
                )
                // Transactional outbox: domain writes + the event row commit atomically; a relay
                // drains `sent_at IS NULL` onto the bus (wired in Step 4).
                s.execute(
                    """CREATE TABLE IF NOT EXISTS accounts.outbox(
                        id         BIGSERIAL PRIMARY KEY,
                        topic      TEXT NOT NULL,
                        payload    JSONB NOT NULL,
                        created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                        sent_at    TIMESTAMPTZ NULL)"""
                )
            }
        }
        println("[accounts] schema ready")
    }

    /** dev-only self-registration. The player row and the outbox row commit in ONE transaction —
     *  the event can neither escape before the write is durable nor survive a rollback. The relay
     *  ([AccountsOutboxRelay]) drains the outbox onto the bus; delivery is async and at-least-once. */
    @Transactional
    fun register(provider: String): UUID {
        val id = UUID.randomUUID()
        Player(id, provider).persist()
        em.createNativeQuery("INSERT INTO accounts.outbox(topic, payload) VALUES (?1, cast(?2 as jsonb))")
            .setParameter(1, PlayerRegistered.TOPIC)
            .setParameter(2, objectMapper.writeValueAsString(PlayerRegistered(id, provider)))
            .executeUpdate()
        return id
    }
}
