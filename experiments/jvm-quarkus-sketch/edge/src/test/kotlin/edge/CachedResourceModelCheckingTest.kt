package edge

import java.util.concurrent.atomic.AtomicInteger
import org.jetbrains.kotlinx.lincheck.annotations.Operation
import org.jetbrains.kotlinx.lincheck.check
import org.jetbrains.kotlinx.lincheck.strategy.managed.modelchecking.ModelCheckingOptions
import org.junit.jupiter.api.Test

/**
 * Lincheck model-checking of [CachedResource] — the double-checked-locking connection cache extracted
 * from `EdgeRemotePlayerCharacters`. This tests the REAL production type (the client USES this class),
 * with a FAKE resource standing in for the QUIC connection so nothing touches the native transport.
 *
 * Model checking (not stress) explores thread interleavings deterministically, so it catches the
 * classic DCL races by construction, not by luck:
 *  - `ensure` drives [CachedResource.get] (which runs `create` at most once), `invalidate` drives
 *    [CachedResource.invalidate] (which runs `close`) — the exact race the reconnect path hits.
 *  - The fake asserts, at creation time, that no other resource is still live: **at most ONE live
 *    resource at any instant**. A broken double-check (two racing `get`s both building) trips this.
 *  - `ensure` returns the resource's generation; Lincheck checks the results are LINEARIZABLE to a
 *    sequential get/invalidate spec — catching lost updates / a second live client / a stale hand-out.
 *  - Model checking also flags any NPE (e.g. a torn `value`) and any deadlock across interleavings.
 */
class CachedResourceModelCheckingTest {

    // Global gauges shared by the fake resources; per-test-instance so Lincheck's sequential reference
    // run (a fresh instance) starts clean. `live` must never exceed 1; `generations` labels each build.
    private val live = AtomicInteger(0)
    private val generations = AtomicInteger(0)

    /** Stands in for the (connection, client) pair: records its own lifecycle, touches no network. */
    private inner class FakeResource {
        val generation: Int = generations.incrementAndGet()

        init {
            val liveNow = live.incrementAndGet()
            check(liveNow == 1) {
                "two live resources at once (live=$liveNow) — double-checked locking let two builds race"
            }
        }

        fun close() {
            live.decrementAndGet()
        }
    }

    private val cache = CachedResource(create = { FakeResource() }, close = { it.close() })

    /** Get-or-build; returns which generation the caller was handed (the observable for linearizability). */
    @Operation
    fun ensure(): Int = cache.get().generation

    /** Drop + close the cached resource, forcing the next [ensure] to build a fresh one. */
    @Operation
    fun invalidate() {
        cache.invalidate()
    }

    @Test
    fun modelChecking() {
        ModelCheckingOptions()
            .threads(2)
            .actorsPerThread(3)
            .check(this::class)
    }
}
