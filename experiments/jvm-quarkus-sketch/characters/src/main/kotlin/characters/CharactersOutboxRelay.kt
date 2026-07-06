package characters

import characters.charactersevents.CharacterCreated
import characters.charactersevents.CharacterDeleted
import io.quarkus.scheduler.Scheduled
import jakarta.enterprise.context.ApplicationScoped
import java.net.URI
import java.net.http.HttpClient
import java.net.http.HttpRequest
import java.net.http.HttpResponse
import java.util.Optional
import javax.sql.DataSource
import org.eclipse.microprofile.config.inject.ConfigProperty
import platform.Outbox
import platform.RoleConfig

/**
 * Drains `characters.outbox` by DIRECT HTTP POST to each topic's subscriber endpoints — no broker,
 * no SmallRye Reactive Messaging. Async events are fire-and-forget fanout: the outbox already gives
 * durability + at-least-once retry, so the relay just re-POSTs the (already-JSON) payload to every
 * subscriber URL, exactly like the sync edge ownerOf and the admin-data REST fan-out cross process
 * boundaries. Transport is identical in both topologies — only INVENTORY_ADDR differs (self in the
 * monolith, process B in the split).
 *
 * Role-gated: all modules share ONE Postgres, so without `isActive("characters")` a process running
 * only, say, the inventory role would ALSO drain characters.outbox and double-publish.
 *
 * Subscriber URLs come from config (`events.subscribers."<topic>"`), comma-separated for multiple
 * subscribers. A topic with NO configured subscriber (e.g. accounts.registered) drains to zero
 * subscribers — delivered-to-nobody is success, so the row is marked sent immediately.
 */
@ApplicationScoped
class CharactersOutboxRelay(
    private val db: DataSource,
    private val roleConfig: RoleConfig,
    @param:ConfigProperty(name = "events.subscribers.\"characters.created\"")
    private val createdSubscribers: Optional<String>,
    @param:ConfigProperty(name = "events.subscribers.\"characters.deleted\"")
    private val deletedSubscribers: Optional<String>,
) {

    private val http: HttpClient = HttpClient.newHttpClient()

    @Scheduled(every = "1s")
    fun drain() {
        if (!roleConfig.isActive("characters")) return
        for (row in Outbox.unsent(db, "characters")) {
            val subscribers = subscribersFor(row.topic)
            // No subscriber for this topic → delivered to zero subscribers is success; mark sent.
            if (subscribers.isEmpty()) {
                Outbox.markSent(db, "characters", row.id)
                continue
            }
            // POST the raw JSON payload to every subscriber. All 2xx → sent; any failure → leave
            // sent_at NULL so the next tick retries (at-least-once; the inbox dedups the redelivery).
            if (postToAll(subscribers, row.payload)) {
                Outbox.markSent(db, "characters", row.id)
            }
        }
    }

    private fun subscribersFor(topic: String): List<String> {
        val configured = when (topic) {
            CharacterCreated.TOPIC -> createdSubscribers
            CharacterDeleted.TOPIC -> deletedSubscribers
            else -> {
                System.err.println("[characters] outbox: unknown topic $topic, no subscribers")
                Optional.empty()
            }
        }
        return configured
            .map { it.split(",").map(String::trim).filter(String::isNotEmpty) }
            .orElse(emptyList())
    }

    /** POST `payload` to each URL; true only if EVERY subscriber returns 2xx. */
    private fun postToAll(urls: List<String>, payload: String): Boolean {
        var allOk = true
        for (url in urls) {
            try {
                val request = HttpRequest.newBuilder(URI.create(url))
                    .header("Content-Type", "application/json")
                    .POST(HttpRequest.BodyPublishers.ofString(payload))
                    .build()
                val response = http.send(request, HttpResponse.BodyHandlers.discarding())
                if (response.statusCode() !in 200..299) {
                    System.err.println("[characters] outbox: $url returned ${response.statusCode()}")
                    allOk = false
                }
            } catch (e: java.io.IOException) {
                System.err.println("[characters] outbox: POST to $url failed: $e")
                allOk = false
            } catch (e: InterruptedException) {
                Thread.currentThread().interrupt()
                System.err.println("[characters] outbox: POST to $url interrupted: $e")
                allOk = false
            }
        }
        return allOk
    }
}
