package characters

import characters.charactersevents.CharacterCreated
import characters.charactersevents.CharacterDeleted
import com.fasterxml.jackson.databind.ObjectMapper
import io.quarkus.runtime.StartupEvent
import jakarta.enterprise.context.ApplicationScoped
import jakarta.enterprise.event.Observes
import jakarta.persistence.EntityManager
import jakarta.transaction.Transactional
import java.util.UUID
import javax.sql.DataSource
import platform.RoleConfig

/**
 * A player has N characters. `player_id` is a PLAIN column — no cross-module FK to accounts.
 * The synchronous `PlayerCharacters` capability is no longer implemented here: the `ownerOf` lookup
 * moved to [LocalPlayerCharacters] and the single capability bean is PRODUCED (transport-transparent
 * local/gRPC) by [PlayerCharactersProvider]. This module still emits Created/Deleted (async) and has
 * no idea `inventory` exists.
 */
@ApplicationScoped
class CharactersModule(
    private val db: DataSource,
    private val em: EntityManager,
    private val objectMapper: ObjectMapper,
    private val roleConfig: RoleConfig,
    private val local: LocalPlayerCharacters,
) {

    /** Default priority — order-independent by construction (own schema, no cross-module FKs). */
    fun migrate(@Observes ev: StartupEvent) {
        if (!roleConfig.isActive("characters")) return
        db.connection.use { c ->
            c.createStatement().use { s ->
                s.execute("CREATE SCHEMA IF NOT EXISTS characters")
                s.execute(
                    """CREATE TABLE IF NOT EXISTS characters.characters(
                        id        BIGSERIAL PRIMARY KEY,
                        player_id UUID NOT NULL,   -- plain ref to accounts' player; NO cross-module FK
                        name      TEXT NOT NULL)"""
                )
                // Transactional outbox: domain writes + the event row commit atomically; a relay
                // drains `sent_at IS NULL` onto the bus (wired in Step 4).
                s.execute(
                    """CREATE TABLE IF NOT EXISTS characters.outbox(
                        id         BIGSERIAL PRIMARY KEY,
                        topic      TEXT NOT NULL,
                        payload    JSONB NOT NULL,
                        created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                        sent_at    TIMESTAMPTZ NULL)"""
                )
            }
        }
        println("[characters] schema ready")
    }

    /** Domain row + outbox row commit in ONE transaction. `flush()` forces the INSERT so the
     *  BIGSERIAL id is assigned before it goes into the event payload; the relay
     *  ([CharactersOutboxRelay]) drains the outbox onto the bus. */
    @Transactional
    fun create(playerId: UUID, name: String): Long {
        val ch = Character(playerId = playerId, name = name)
        ch.persist()
        em.flush()   // assign the IDENTITY id before it enters the outbox payload
        val id = ch.id!!
        appendOutbox(CharacterCreated.TOPIC, CharacterCreated(id, playerId, name))
        return id
    }

    @Transactional
    fun delete(id: Long) {
        val playerId = local.ownerOf(id) ?: return
        Character.deleteById(id)
        // Integrity across modules comes from THIS event, not an FK cascade.
        appendOutbox(CharacterDeleted.TOPIC, CharacterDeleted(id, playerId))
    }

    /** Insert one outbox row in the CURRENT transaction (same EntityManager, hence atomic with the
     *  domain write). Payload is the event serialized to JSON. */
    private fun appendOutbox(topic: String, payload: Any) {
        em.createNativeQuery("INSERT INTO characters.outbox(topic, payload) VALUES (?1, cast(?2 as jsonb))")
            .setParameter(1, topic)
            .setParameter(2, objectMapper.writeValueAsString(payload))
            .executeUpdate()
    }
}
