package characters.charactersevents

import java.util.UUID

/** Routed by type (see accountsevents). Payload shape = the published contract. */
data class CharacterCreated(val characterId: Long, val playerId: UUID, val name: String)
data class CharacterDeleted(val characterId: Long, val playerId: UUID)
