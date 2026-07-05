package characters.charactersevents

import io.quarkus.runtime.annotations.RegisterForReflection
import java.util.UUID

/**
 * Routed by type (see accountsevents). Payload shape = the published contract. Each payload now
 * carries an explicit [TOPIC] (its bus channel) and @RegisterForReflection for JSON serde under
 * native-image.
 */
@RegisterForReflection
public data class CharacterCreated(val characterId: Long, val playerId: UUID, val name: String) {
    public companion object {
        public const val TOPIC: String = "characters.created"
    }
}

@RegisterForReflection
public data class CharacterDeleted(val characterId: Long, val playerId: UUID) {
    public companion object {
        public const val TOPIC: String = "characters.deleted"
    }
}
