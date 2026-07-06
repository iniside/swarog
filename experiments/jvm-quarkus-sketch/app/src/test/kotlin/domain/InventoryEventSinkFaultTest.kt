package domain

import characters.charactersevents.CharacterCreated
import inventory.InventoryModule
import io.quarkus.test.InjectMock
import io.quarkus.test.junit.QuarkusTest
import io.restassured.RestAssured.given
import io.restassured.http.ContentType
import java.util.UUID
import org.junit.jupiter.api.Test
import org.mockito.Mockito

/**
 * P3-REST fault path: the "relay retries on non-2xx" contract. The characters outbox relay treats any
 * non-2xx as a delivery failure and re-POSTs (at-least-once). So if [inventory.InventoryEventSink]'s
 * handler throws, the sink MUST let it surface as a 500 — swallowing it to a 200 would tell the relay
 * "delivered", silently dropping the event.
 *
 * [InventoryModule] is replaced with a Mockito mock whose `onCharacterCreated` throws, so the failure is
 * a pure handler fault with no DB writes (nothing to clean up). This is a separate class from the happy
 * path because @InjectMock alters the bean container (its own boot). Deliberate-break proof: wrapping the
 * sink body in `try { … } catch (e) { Response.ok() }` makes this assertion RED.
 */
@QuarkusTest
class InventoryEventSinkFaultTest {

    @InjectMock
    lateinit var inventory: InventoryModule

    @Test
    fun `a throwing handler surfaces as 500, the sink does not swallow it to 200`() {
        Mockito.doThrow(RuntimeException("boom in handler"))
            .`when`(inventory).onCharacterCreated(anyCreated())

        given()
            .contentType(ContentType.JSON)
            .body("""{"characterId":1,"playerId":"${UUID.randomUUID()}","name":"Boom"}""")
            .`when`().post("/events/character-created")
            .then().statusCode(500)
    }

    /** Registers Mockito's `any()` matcher but returns a non-null placeholder, so Kotlin's non-null
     *  parameter check does not NPE while stubbing (`Mockito.any()` itself returns null). Mockito uses
     *  the registered matcher, not the returned value. */
    @Suppress("IgnoredReturnValue") // Mockito.any()'s return value is unused by design — see doc above
    private fun anyCreated(): CharacterCreated {
        Mockito.any(CharacterCreated::class.java)
        return CharacterCreated(0L, UUID(0L, 0L), "")
    }
}
