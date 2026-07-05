package core

/**
 * A feature. Self-registers into the [Registry]; the core never imports a module —
 * modules import the core. Dependency only ever points module -> core.
 *
 * Use a class with state (db, bus, caches) — the Kotlin analogue of Go's pointer receiver.
 */
interface Module {
    val name: String

    /** Declared SYNC dependencies (service-registry needs). Must match real downward deps. */
    val dependsOn: List<String> get() = emptyList()

    /** Wire only — NO I/O. Provide services, subscribe to the bus, stash ctx handles. */
    fun init(ctx: Context)
}

/** Optional: owns its OWN schema. Runs after every init(), before any start(). */
interface Migrator {
    fun migrate(ctx: Context)
}

/** Optional: background work / resources. Runs after migrate, in dependency order. */
interface Starter {
    fun start(ctx: Context)
}

/** Optional: teardown. Runs in REVERSE dependency order on shutdown. */
interface Stopper {
    fun stop()
}
