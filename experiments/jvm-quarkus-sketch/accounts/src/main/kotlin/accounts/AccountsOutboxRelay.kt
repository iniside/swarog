package accounts

import accounts.accountsevents.PlayerRegistered
import com.fasterxml.jackson.databind.ObjectMapper
import io.quarkus.scheduler.Scheduled
import io.smallrye.reactive.messaging.MutinyEmitter
import jakarta.enterprise.context.ApplicationScoped
import javax.sql.DataSource
import org.eclipse.microprofile.reactive.messaging.Channel
import platform.Outbox
import platform.RoleConfig

/**
 * Drains `accounts.outbox` onto the bus. One [MutinyEmitter] per owned topic (accounts owns one:
 * PlayerRegistered) — an emitter is statically bound to its channel name, so the relay lives here,
 * not in `platform`.
 *
 * Role-gated: all modules share ONE Postgres, so without `isActive("accounts")` a process that
 * only runs some OTHER role would still poll and drain this schema's outbox. The channel name is the
 * TOPIC constant, transport-agnostic: internal in the monolith, Kafka once Step 7 adds a connector.
 */
@ApplicationScoped
class AccountsOutboxRelay(
    private val db: DataSource,
    private val objectMapper: ObjectMapper,
    private val roleConfig: RoleConfig,
    @Channel(PlayerRegistered.TOPIC) private val registeredEmitter: MutinyEmitter<PlayerRegistered>,
) {

    @Scheduled(every = "1s")
    fun drain() {
        if (!roleConfig.isActive("accounts")) return
        for (row in Outbox.unsent(db, "accounts")) {
            try {
                when (row.topic) {
                    PlayerRegistered.TOPIC ->
                        registeredEmitter.sendAndAwait(objectMapper.readValue(row.payload, PlayerRegistered::class.java))
                    else -> {
                        System.err.println("[accounts] outbox: unknown topic ${row.topic}, skipping row ${row.id}")
                        continue
                    }
                }
                Outbox.markSent(db, "accounts", row.id)
            } catch (e: Exception) {
                // Leave sent_at NULL — the next tick retries (at-least-once). Never mark on failure.
                System.err.println("[accounts] outbox relay failed for row ${row.id} (${row.topic}): $e")
            }
        }
    }
}
