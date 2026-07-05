package characters.charactersapi

/**
 * Wire DTOs for the `characters.ownerOf` edge-RPC method (msquic/QUIC transport in a split topology).
 * Plain MessagePack-serializable data classes — the edge codec reflects over them, no schema/codegen.
 *
 * They live in `characters-api` (not `edge`) on purpose: both `characters` (server handler) and
 * `inventory` (client caller) already depend on `characters-api`, while `edge` does NOT depend on
 * `characters-api`, so there is no dependency cycle. `edge` stays a transport-agnostic leaf.
 */
public data class OwnerOfRequest(val characterId: Long)

public data class OwnerOfReply(val found: Boolean, val ownerId: String?)
