package accounts.accountsevents

import core.Topic
import java.util.UUID

/**
 * The ONLY surface `accounts` shares for others to react to. Evolve additively
 * (new field / a RegisteredV2 topic) — never mutate a published payload shape.
 */
data class PlayerRegistered(val playerId: UUID, val provider: String)

val PlayerRegisteredTopic = Topic<PlayerRegistered>("accounts.player_registered")
