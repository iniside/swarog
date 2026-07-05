package characters.charactersapi

import java.util.UUID

/**
 * The synchronous capability `characters` publishes for other modules. Same nominal-typing note
 * as the framework-free sketch: the contract needs a published home because the JVM matches
 * interfaces by name, not structure.
 *
 * vs the framework-free sketch: `ctx.provide/require` is gone — CDI resolves this BY TYPE.
 * `CharactersModule` implements it, `inventory` constructor-injects it; neither names the other.
 */
interface PlayerCharacters {
    /** @return the owning player's id, or null if no such character exists. */
    fun ownerOf(characterId: Long): UUID?
}
