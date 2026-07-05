package core

import javax.sql.DataSource
import kotlin.reflect.KClass

/**
 * The wiring surface handed to each module's [Module.init].
 *
 *  - [provide] / [requireService] : the SYNCHRONOUS service registry (single-value).
 *        Ask B now, get an answer. The consumer asserts the service to its OWN interface,
 *        so it depends on a capability, not a package.
 *  - [bus] : the ASYNC event bus — the default glue for sideways reactions.
 *  - [db]  : one shared Postgres. Each module owns its OWN schema and touches no other
 *        module's tables. No cross-module foreign keys.
 */
class Context(val db: DataSource, val bus: Bus = Bus()) {
    private val services = HashMap<KClass<*>, Any>()

    fun <T : Any> provide(type: KClass<T>, impl: T) {
        require(services.put(type, impl) == null) { "service already provided: ${type.simpleName}" }
    }

    @Suppress("UNCHECKED_CAST")
    fun <T : Any> requireService(type: KClass<T>): T =
        (services[type] ?: error("no service registered for ${type.simpleName}")) as T

    // --- multi-value contribution registry (the minor 4th seam) ---
    private val slots = HashMap<String, MutableList<Any?>>()

    /** Many modules contribute to a [slot]; one consumer reads them all via [contributions].
     *  A new contributor appears without the consumer being edited. */
    fun <T> contribute(slot: Slot<T>, value: T) {
        slots.getOrPut(slot.name) { mutableListOf() }.add(value)
    }

    @Suppress("UNCHECKED_CAST")
    fun <T> contributions(slot: Slot<T>): List<T> =
        (slots[slot.name]?.toList() ?: emptyList<Any?>()) as List<T>
}

/**
 * Reified sugar: `ctx.require<PlayerCharacters>()` instead of Java's `ctx.require(PlayerCharacters.class)`.
 * The service-registry seam reads cleaner here than on either Go or plain Java.
 */
inline fun <reified T : Any> Context.require(): T = requireService(T::class)
