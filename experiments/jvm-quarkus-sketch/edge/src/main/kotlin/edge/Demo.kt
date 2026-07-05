package edge

/**
 * Self-contained demo request/reply/push types + a handler registration, proving the RPC path end
 * to end. Kept self-contained (NOT wired to characters-api) on purpose: `characters-api`'s only
 * capability is `ownerOf(id) -> playerId`, not a character LIST, so depending on it would add a
 * coupling that buys nothing for THIS increment. The point is the transport-agnostic path, not a
 * real query — so `characters.list` returns a canned list. A later step swaps the stub body for a
 * real gRPC/REST call into the characters module without touching the RPC core.
 */

data class ListCharactersRequest(val playerId: String)

data class CharactersReply(val names: List<String>)

/** Server-push payload — e.g. broadcast when a character is created elsewhere in the backend. */
data class CharacterCreatedPush(val playerId: String, val name: String)

object EdgeDemo {
    /** Registers the demo methods on a router: a working `characters.list` and a throwing one. */
    fun register(router: EdgeRouter, codec: EdgeCodec) {
        router.register(
            "characters.list",
            codec.typedHandler<ListCharactersRequest, CharactersReply> { req ->
                // STUB: canned roster keyed off the (ignored) playerId — no DB, no characters module.
                require(req.playerId.isNotBlank()) { "playerId is required" }
                CharactersReply(names = listOf("Aria", "Borin", "Cael"))
            },
        )
        router.register(
            "characters.boom",
            codec.typedHandler<ListCharactersRequest, CharactersReply> {
                throw IllegalStateException("character service unavailable")
            },
        )
    }
}
