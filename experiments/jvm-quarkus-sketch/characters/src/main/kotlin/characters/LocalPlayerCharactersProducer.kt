package characters

import characters.charactersapi.PlayerCharacters
import jakarta.enterprise.context.ApplicationScoped
import jakarta.enterprise.inject.Produces
import java.util.UUID

/**
 * The LOCAL [PlayerCharacters] producer — the in-process branch of what used to be
 * `PlayerCharactersProvider`. It no longer branches on RoleConfig: bean PRESENCE decides now. Any
 * service that includes the `characters` impl (the monolith `app`, or the split `characters-service`)
 * gets this producer, which delegates straight to [LocalPlayerCharacters]. A service that does NOT
 * host `characters` (the split `inventory-service`) instead includes `characters-client`, whose
 * producer dials the remote QUIC server. Each topology therefore has EXACTLY ONE `PlayerCharacters`
 * producer — no ambiguous resolution.
 *
 * [LocalPlayerCharacters] is a CONCRETE bean that does NOT implement [PlayerCharacters], so it never
 * competes with the produced capability bean — the single `PlayerCharacters` in this process is this
 * producer's return value.
 */
@ApplicationScoped
class LocalPlayerCharactersProducer(
    private val local: LocalPlayerCharacters,
) {
    @Produces
    @ApplicationScoped
    fun playerCharacters(): PlayerCharacters = LocalPlayerCharactersAdapter(local)
}

/** In-process: delegate straight to the concrete local capability. */
private class LocalPlayerCharactersAdapter(
    private val local: LocalPlayerCharacters,
) : PlayerCharacters {
    override fun ownerOf(characterId: Long): UUID? = local.ownerOf(characterId)
}
