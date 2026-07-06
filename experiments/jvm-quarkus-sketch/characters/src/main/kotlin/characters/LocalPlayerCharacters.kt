package characters

import characters.charactersapi.CharacterSummary
import jakarta.enterprise.context.ApplicationScoped
import java.util.UUID

/**
 * The in-process character reads, extracted out of [CharactersModule]. Deliberately a CONCRETE bean
 * that does NOT implement [characters.charactersapi.PlayerCharacters]: the single capability bean is
 * PRODUCED (see [PlayerCharactersProvider]) so there is exactly one `PlayerCharacters` in every role
 * combination. Both the local delegate and the edge-RPC QUIC server ([CharactersEdgeServer]) fan in here.
 *
 * Panache reads here need an active Hibernate session, so callers outside a request/transaction context
 * (the edge server's per-connection worker) wrap them in a programmatic transaction — see
 * [CharactersEdgeServer].
 */
@ApplicationScoped
class LocalPlayerCharacters {
    fun ownerOf(characterId: Long): UUID? = Character.findById(characterId)?.playerId

    /** All characters owned by [playerId], projected to the wire summary — backs `characters.list`. */
    fun charactersOf(playerId: UUID): List<CharacterSummary> =
        Character.list("playerId = ?1", playerId)
            .map { CharacterSummary(checkNotNull(it.id) { "persisted character has no id" }, it.name) }
}
