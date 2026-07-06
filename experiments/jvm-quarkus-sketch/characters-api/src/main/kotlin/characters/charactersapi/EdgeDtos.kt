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

/**
 * Wire DTOs for the PLAYER-FACING `characters.list` edge-RPC method: given a player id, return that
 * player's characters. Unlike `ownerOf` (an internal inventory→characters seam), this is a read a game
 * client would call directly, so it is the kind of method a QUIC gateway fronts. [playerId] is the
 * UUID as text — msgpack carries it as a plain string, matching how [OwnerOfReply.ownerId] is encoded.
 */
public data class ListCharactersRequest(val playerId: String)

public data class CharacterSummary(val id: Long, val name: String)

public data class ListCharactersReply(val characters: List<CharacterSummary>)
