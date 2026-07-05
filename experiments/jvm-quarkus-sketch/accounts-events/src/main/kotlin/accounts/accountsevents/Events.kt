package accounts.accountsevents

import io.quarkus.runtime.annotations.RegisterForReflection
import java.util.UUID

/**
 * The ONLY surface `accounts` shares for others to react to. Evolve additively
 * (new field / a RegisteredV2 type) — never mutate a published payload shape.
 *
 * vs the framework-free sketch: there is no `Topic` descriptor anymore. CDI events are
 * routed by the PAYLOAD TYPE itself (`Event<PlayerRegistered>.fireAsync` -> `@ObservesAsync
 * PlayerRegistered`), so the payload class IS the topic. As this payload becomes a WIRE
 * contract (JSON over the bus), [TOPIC] names its channel and @RegisterForReflection keeps
 * Jackson serde working under native-image.
 */
@RegisterForReflection
public data class PlayerRegistered(val playerId: UUID, val provider: String) {
    public companion object {
        public const val TOPIC: String = "accounts.registered"
    }
}
