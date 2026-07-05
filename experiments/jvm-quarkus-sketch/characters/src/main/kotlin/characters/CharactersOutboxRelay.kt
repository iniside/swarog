package characters

import characters.charactersevents.CharacterCreated
import characters.charactersevents.CharacterDeleted
import com.fasterxml.jackson.databind.ObjectMapper
import io.quarkus.scheduler.Scheduled
import io.smallrye.reactive.messaging.MutinyEmitter
import jakarta.enterprise.context.ApplicationScoped
import javax.sql.DataSource
import org.eclipse.microprofile.reactive.messaging.Channel
import platform.Outbox
import platform.RoleConfig

/**
 * Drains `characters.outbox` onto the bus. One [MutinyEmitter] per owned topic (characters owns two:
 * CharacterCreated, CharacterDeleted) — an emitter is statically bound to its channel name, so the
 * relay lives here, not in `platform`.
 *
 * Role-gated: all modules share ONE Postgres, so without `isActive("characters")` a process running
 * only, say, the inventory role would ALSO drain characters.outbox and double-publish. Channel names
 * are the TOPIC constants, transport-agnostic: internal in the monolith, Kafka once Step 7 adds a
 * connector.
 */
@ApplicationScoped
class CharactersOutboxRelay(
    private val db: DataSource,
    private val objectMapper: ObjectMapper,
    private val roleConfig: RoleConfig,
    @Channel(CharacterCreated.TOPIC) private val createdEmitter: MutinyEmitter<CharacterCreated>,
    @Channel(CharacterDeleted.TOPIC) private val deletedEmitter: MutinyEmitter<CharacterDeleted>,
) {

    @Scheduled(every = "1s")
    fun drain() {
        if (!roleConfig.isActive("characters")) return
        for (row in Outbox.unsent(db, "characters")) {
            try {
                when (row.topic) {
                    CharacterCreated.TOPIC ->
                        createdEmitter.sendAndAwait(objectMapper.readValue(row.payload, CharacterCreated::class.java))
                    CharacterDeleted.TOPIC ->
                        deletedEmitter.sendAndAwait(objectMapper.readValue(row.payload, CharacterDeleted::class.java))
                    else -> {
                        System.err.println("[characters] outbox: unknown topic ${row.topic}, skipping row ${row.id}")
                        continue
                    }
                }
                Outbox.markSent(db, "characters", row.id)
            } catch (e: Exception) {
                // Leave sent_at NULL — the next tick retries (at-least-once). Never mark on failure.
                System.err.println("[characters] outbox relay failed for row ${row.id} (${row.topic}): $e")
            }
        }
    }
}
