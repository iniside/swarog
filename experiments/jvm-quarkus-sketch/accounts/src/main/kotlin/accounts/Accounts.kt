package accounts

import accounts.accountsevents.PlayerRegistered
import io.quarkus.narayana.jta.QuarkusTransaction
import io.quarkus.runtime.StartupEvent
import jakarta.enterprise.context.ApplicationScoped
import jakarta.enterprise.event.Event
import jakarta.enterprise.event.Observes
import java.util.UUID
import javax.sql.DataSource
import platform.RoleConfig

/** Owns player identity. Foundation module — depends on nothing but the container. */
@ApplicationScoped
class AccountsModule(
    private val db: DataSource,
    private val registered: Event<PlayerRegistered>,
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

    /** dev-only self-registration. Emits PlayerRegistered — async, fire-and-forget.
     *
     *  The write runs in a PROGRAMMATIC transaction so the event fires AFTER commit — with plain
     *  `@Transactional` on this method, `fireAsync` would leak the event to observers before the
     *  row is durable (and a rollback would leave a phantom event). The raw-JDBC version got this
     *  ordering for free from autocommit. */
    fun register(provider: String): UUID {
        val id = UUID.randomUUID()
        QuarkusTransaction.requiringNew().run { Player(id, provider).persist() }
        registered.fireAsync(PlayerRegistered(id, provider))
            .whenComplete { _, e -> if (e != null) System.err.println("event handler failed for PlayerRegistered: $e") }
        return id
    }
}
