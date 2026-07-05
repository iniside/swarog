package characters.charactersevents

import io.quarkus.runtime.annotations.RegisterForReflection
import java.util.UUID

/**
 * Routed by type (see accountsevents). Payload shape = the published contract. Each payload now
 * carries an explicit [TOPIC] (its bus channel) and @RegisterForReflection for JSON serde under
 * native-image.
 */
@RegisterForReflection
data class CharacterCreated(val characterId: Long, val playerId: UUID, val name: String) {
    companion object {
        const val TOPIC = "characters.created"
    }
}

@RegisterForReflection
data class CharacterDeleted(val characterId: Long, val playerId: UUID) {
    companion object {
        const val TOPIC = "characters.deleted"
    }
}
