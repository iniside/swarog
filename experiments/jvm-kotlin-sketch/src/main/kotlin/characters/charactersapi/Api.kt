package characters.charactersapi

import java.util.UUID

/**
 * The synchronous capability `characters` publishes for other modules (e.g. inventory
 * authorizing a character's inventory).
 *
 * WHY THIS PACKAGE EXISTS — the one real Go->JVM difference:
 * Go's service registry lets the CONSUMER define the interface and relies on STRUCTURAL
 * typing to match a provider's concrete type. Kotlin/JVM is NOMINALLY typed: an impl must
 * explicitly declare the interface it satisfies. So the contract lives in a tiny published
 * `charactersapi` package (the sync analogue of `charactersevents`). Consumers depend on
 * THIS capability, never on the `characters` implementation.
 */
interface PlayerCharacters {
    /** @return the owning player's id, or null if no such character exists. */
    fun ownerOf(characterId: Long): UUID?
}
