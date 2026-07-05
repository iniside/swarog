package characters

import characters.grpc.MutinyPlayerCharactersGrpcGrpc
import characters.grpc.OwnerOfReply
import characters.grpc.OwnerOfRequest
import io.quarkus.grpc.GrpcService
import io.smallrye.common.annotation.Blocking
import io.smallrye.mutiny.Uni

/**
 * The gRPC server side of the `ownerOf` capability — the same [LocalPlayerCharacters] lookup, exposed
 * over the wire so a process that does NOT host `characters` can still ask (via the gRPC client
 * adapter in [PlayerCharactersProvider]). Only the process running the `characters` role starts this
 * server (gated by config profile in a later step). `@Blocking` moves the Panache read off the gRPC
 * event loop onto a worker thread.
 */
@GrpcService
class PlayerCharactersGrpcService(
    private val local: LocalPlayerCharacters,
) : MutinyPlayerCharactersGrpcGrpc.PlayerCharactersGrpcImplBase() {

    @Blocking
    override fun ownerOf(request: OwnerOfRequest): Uni<OwnerOfReply> {
        val owner = local.ownerOf(request.characterId.toLong())
        return Uni.createFrom().item(
            OwnerOfReply.newBuilder()
                .setFound(owner != null)
                .setOwnerId(owner?.toString() ?: "")
                .build(),
        )
    }
}
