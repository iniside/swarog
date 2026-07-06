package accounts

import accounts.accountsevents.PlayerRegistered
import com.fasterxml.jackson.module.kotlin.jacksonObjectMapper
import com.fasterxml.jackson.module.kotlin.readValue
import java.util.UUID
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

/**
 * P1-ACCOUNTS (pure-unit): [PlayerRegistered] is a WIRE contract — it is serialized into
 * `accounts.outbox.payload` (JSONB) and POSTed to subscribers as JSON. This locks the round-trip
 * (`serialize -> deserialize -> equals`) so an accidental field rename / type change / non-serializable
 * addition fails HERE at the contract, not silently on the wire. Uses the Kotlin-module ObjectMapper,
 * the same shape Quarkus registers in production.
 */
class PlayerRegisteredJsonTest {

    private val mapper = jacksonObjectMapper()

    @Test
    fun `PlayerRegistered round-trips through JSON unchanged`() {
        val original = PlayerRegistered(UUID.randomUUID(), "epic")

        val json = mapper.writeValueAsString(original)
        val restored = mapper.readValue<PlayerRegistered>(json)

        assertEquals(original, restored)
    }
}
