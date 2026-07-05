package characters

import characters.charactersapi.PlayerCharacters
import characters.grpc.MutinyPlayerCharactersGrpcGrpc.MutinyPlayerCharactersGrpcStub
import characters.grpc.OwnerOfRequest
import io.quarkus.grpc.GrpcClient
import jakarta.enterprise.context.ApplicationScoped
import jakarta.enterprise.inject.Produces
import java.util.UUID
import platform.RoleConfig

/**
 * The ONE place a [PlayerCharacters] bean comes from — transport-transparent by construction:
 *  - a process that HOSTS `characters` gets a local delegate straight to [LocalPlayerCharacters];
 *  - a process that does NOT gets a gRPC adapter over the `@GrpcClient` stub → the remote server.
 *
 * Living in the `characters` impl (not `platform`/`inventory`) is deliberate: it keeps the choice next
 * to the capability it fronts and avoids an impl-on-impl / Gradle cycle. Exactly one bean of type
 * [PlayerCharacters] exists in every role combination — [LocalPlayerCharacters] is a concrete type
 * (not a `PlayerCharacters`), and the generated gRPC stub is a different type — so no ambiguous
 * resolution. The `@GrpcClient` channel is lazy (connects on first call): injected but inert in the
 * monolith, where the local branch is taken.
 */
@ApplicationScoped
class PlayerCharactersProvider(
    private val roleConfig: RoleConfig,
    private val local: LocalPlayerCharacters,
    @GrpcClient("characters") private val remote: MutinyPlayerCharactersGrpcStub,
) {
    @Produces
    @ApplicationScoped
    fun playerCharacters(): PlayerCharacters =
        if (roleConfig.isActive("characters")) LocalPlayerCharactersAdapter(local)
        else GrpcPlayerCharactersAdapter(remote)
}

/** In-process: delegate straight to the concrete local capability. */
private class LocalPlayerCharactersAdapter(
    private val local: LocalPlayerCharacters,
) : PlayerCharacters {
    override fun ownerOf(characterId: Long): UUID? = local.ownerOf(characterId)
}

/**
 * Cross-process: bridge the reactive gRPC reply back to the synchronous [PlayerCharacters] contract.
 * `.await().indefinitely()` blocks the calling thread — safe only OFF the event loop, hence callers
 * that reach a character inventory write must be `@Blocking` (e.g. `InventoryResource`).
 */
private class GrpcPlayerCharactersAdapter(
    private val stub: MutinyPlayerCharactersGrpcStub,
) : PlayerCharacters {
    override fun ownerOf(characterId: Long): UUID? {
        val reply = stub.ownerOf(
            OwnerOfRequest.newBuilder().setCharacterId(characterId.toString()).build(),
        ).await().indefinitely()
        return if (reply.found) UUID.fromString(reply.ownerId) else null
    }
}
