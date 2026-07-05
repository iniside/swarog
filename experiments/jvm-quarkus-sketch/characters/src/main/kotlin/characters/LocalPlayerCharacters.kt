package characters

import jakarta.enterprise.context.ApplicationScoped
import java.util.UUID

/**
 * The in-process `ownerOf` lookup, extracted out of [CharactersModule]. Deliberately a CONCRETE bean
 * that does NOT implement [characters.charactersapi.PlayerCharacters]: the single capability bean is
 * PRODUCED (see [PlayerCharactersProvider]) so there is exactly one `PlayerCharacters` in every role
 * combination. Both the local delegate and the gRPC service ([PlayerCharactersGrpcService]) fan in here.
 */
@ApplicationScoped
class LocalPlayerCharacters {
    fun ownerOf(characterId: Long): UUID? = Character.findById(characterId)?.playerId
}
