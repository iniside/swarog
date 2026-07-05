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
public interface PlayerCharacters {
    /**
     * @return the owning player's id, or null if no such character exists.
     * @throws CharactersUnavailableException if a REMOTE provider cannot be reached — a transport
     *   failure is NOT the same answer as null ("no such character"). The in-process implementation
     *   never throws it.
     */
    public fun ownerOf(characterId: Long): UUID?
}

/**
 * Thrown by a REMOTE [PlayerCharacters] when the provider cannot be reached (transport failure) —
 * DISTINCT from `ownerOf` returning null, which means "no such character". Consumers map this to
 * 503 (upstream unavailable), never a false 400/404. The local (in-process) implementation never
 * throws it, so the monolith never takes this path. This mirrors the Go seam widening `ownerOf`
 * with an `error` so a dead provider is not indistinguishable from "not owned".
 */
public class CharactersUnavailableException(message: String, cause: Throwable? = null) :
    RuntimeException(message, cause)
