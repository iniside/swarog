package inventory

/**
 * Wire DTOs for the PLAYER-FACING `inventory.list` edge-RPC method: given an owner, return its holdings.
 *
 * WHY HERE (not a contract module like `characters-api`): the `characters.ownerOf`/`characters.list`
 * DTOs live in `characters-api` because BOTH the characters impl (server) AND a real cross-module
 * production client (`inventory` / `characters-client`) share them. `inventory.list` has NO such
 * cross-module production client — the ONLY server is this module, and the ONLY relay (the gateway)
 * byte-relays the blob without ever decoding it. A whole new `inventory-api` Gradle module for two data
 * classes with no shared production consumer would be ceremony with no payoff, so the inventory edge
 * DTOs live next to [InventoryEdgeServer]. A test player client that decodes replies defines its own
 * structurally-identical types (msgpack is schemaless), exactly as `MsQuicForwardSmokeTest` does.
 *
 * [ownerType]/[ownerId] mirror [Owner]; the handler reconstructs `Owner(OwnerType.valueOf(ownerType),
 * ownerId)`.
 */
data class ListHoldingsRequest(val ownerType: String, val ownerId: String)

data class HoldingLine(val item: String, val qty: Int)

data class ListHoldingsReply(val holdings: List<HoldingLine>)
