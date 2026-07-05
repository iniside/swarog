package accounts

import accounts.accountsevents.PlayerRegistered
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
 * Drains `accounts.outbox` by DIRECT HTTP POST to each topic's subscriber endpoints — no broker, no
 * SmallRye Reactive Messaging. See [CharactersOutboxRelay] for the transport rationale.
 *
 * accounts.registered has NO consumer today, so no `events.subscribers."accounts.registered"` entry
 * exists: the relay drains to zero subscribers and marks the row sent immediately (a durable log for
 * future consumers). A subscriber is added later with a single config line — no code change here.
 *
 * Role-gated: all modules share ONE Postgres, so without `isActive("accounts")` a process that only
 * runs some OTHER role would still poll and drain this schema's outbox.
 */
@ApplicationScoped
class AccountsOutboxRelay(
    private val db: DataSource,
    private val roleConfig: RoleConfig,
    @param:ConfigProperty(name = "events.subscribers.\"accounts.registered\"")
    private val registeredSubscribers: Optional<String>,
) {

    private val http: HttpClient = HttpClient.newHttpClient()

    @Scheduled(every = "1s")
    fun drain() {
        if (!roleConfig.isActive("accounts")) return
        for (row in Outbox.unsent(db, "accounts")) {
            val subscribers = subscribersFor(row.topic)
            if (subscribers.isEmpty()) {
                Outbox.markSent(db, "accounts", row.id)
                continue
            }
            if (postToAll(subscribers, row.payload)) {
                Outbox.markSent(db, "accounts", row.id)
            }
        }
    }

    private fun subscribersFor(topic: String): List<String> {
        val configured = when (topic) {
            PlayerRegistered.TOPIC -> registeredSubscribers
            else -> {
                System.err.println("[accounts] outbox: unknown topic $topic, no subscribers")
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
                    System.err.println("[accounts] outbox: $url returned ${response.statusCode()}")
                    allOk = false
                }
            } catch (e: Exception) {
                System.err.println("[accounts] outbox: POST to $url failed: $e")
                allOk = false
            }
        }
        return allOk
    }
}
