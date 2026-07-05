package characters.charactersevents

import core.Topic
import java.util.UUID

data class CharacterCreated(val characterId: Long, val playerId: UUID, val name: String)
data class CharacterDeleted(val characterId: Long, val playerId: UUID)

val CharacterCreatedTopic = Topic<CharacterCreated>("characters.created")
val CharacterDeletedTopic = Topic<CharacterDeleted>("characters.deleted")
