package core

import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicInteger

/**
 * Typed event descriptor — the Kotlin analogue of Go's `core.Define[T]("topic")`.
 * Each publishing domain owns its `<module>.events` package declaring these.
 */
data class Topic<T>(val name: String)

/**
 * The default glue: async + fire-and-forget. [emit] never blocks and returns nothing,
 * so it can't deliver a synchronous answer — that's a service interface's job (see [Context]).
 * State projected from events is eventually consistent.
 *
 * Handlers run on virtual threads (Project Loom, JDK 21+) — one cheap thread per delivery,
 * the JVM analogue of firing a goroutine per event.
 */
class Bus {
    private val subs = ConcurrentHashMap<String, CopyOnWriteArrayList<(Any?) -> Unit>>()
    private val pool = Executors.newVirtualThreadPerTaskExecutor()
    private val inFlight = AtomicInteger(0)

    fun <T> on(topic: Topic<T>, handler: (T) -> Unit) {
        @Suppress("UNCHECKED_CAST")
        subs.getOrPut(topic.name) { CopyOnWriteArrayList() }.add(handler as (Any?) -> Unit)
    }

    fun <T> emit(topic: Topic<T>, payload: T) {
        val handlers = subs[topic.name] ?: return
        for (h in handlers) {
            inFlight.incrementAndGet()
            pool.submit {
                try {
                    h(payload)
                } catch (e: Throwable) {
                    System.err.println("bus handler failed for '${topic.name}': $e")
                } finally {
                    inFlight.decrementAndGet()
                }
            }
        }
    }

    /** Test/demo helper: block until in-flight handlers settle (eventual consistency made observable). */
    fun awaitIdle(timeoutMs: Long) {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (inFlight.get() > 0 && System.currentTimeMillis() < deadline) Thread.sleep(5)
    }

    /** Shutdown: stop accepting, let in-flight deliveries finish (drain). */
    fun drain() {
        pool.shutdown()
    }
}
