package accounts.accountsevents

import java.util.UUID

/**
 * The ONLY surface `accounts` shares for others to react to. Evolve additively
 * (new field / a RegisteredV2 type) — never mutate a published payload shape.
 *
 * vs the framework-free sketch: there is no `Topic` descriptor anymore. CDI events are
 * routed by the PAYLOAD TYPE itself (`Event<PlayerRegistered>.fireAsync` -> `@ObservesAsync
 * PlayerRegistered`), so the payload class IS the topic.
 */
data class PlayerRegistered(val playerId: UUID, val provider: String)
