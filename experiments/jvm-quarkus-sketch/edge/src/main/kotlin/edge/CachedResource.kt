package edge

/**
 * A lazily-created, invalidatable single resource guarded by double-checked locking — the exact shape
 * the edge client needs for its cached QUIC connection: build the live resource once on first use, hand
 * the SAME instance to every subsequent caller, and on failure drop it so the next caller rebuilds.
 *
 * Concurrency contract (the whole reason this is its own type — a hand-rolled DCL cache is notoriously
 * easy to get subtly wrong, so it is extracted here and model-checked in isolation by Lincheck):
 *  - [get] returns the cached value, creating it exactly once under [lock] via [create]; the volatile
 *    fast-path avoids the monitor once the value exists.
 *  - [invalidate] closes the current value (if any) via [close] and clears the cache, so the next [get]
 *    rebuilds a FRESH value.
 *  - At most ONE value is ever live at a time (create is never entered while a value is still cached),
 *    and no value is created concurrently with another — both guaranteed by the inner re-check under the
 *    lock. Drop the inner re-check and two racing [get]s both build → two live values: the classic bug.
 *
 * [create] and [close] run under the lock; [create] may block (the real one dials QUIC). [close] is
 * best-effort — a throwing close still clears the cache, so a dead resource can never wedge the cache.
 */
class CachedResource<T : Any>(
    private val create: () -> T,
    private val close: (T) -> Unit,
) {
    private val lock = Any()

    @Volatile private var value: T? = null

    /** The cached value, building it once on first call. Subsequent calls return the same instance. */
    fun get(): T {
        value?.let { return it }
        synchronized(lock) {
            value?.let { return it }
            val v = create()
            value = v
            return v
        }
    }

    /** Closes and drops the cached value (best-effort close) so the next [get] rebuilds a fresh one. */
    fun invalidate() {
        synchronized(lock) {
            value?.let { v -> runCatching { close(v) } }
            value = null
        }
    }
}
