package edge.msquic

import java.lang.foreign.Arena
import java.lang.foreign.FunctionDescriptor
import java.lang.foreign.Linker
import java.lang.foreign.MemorySegment
import java.lang.foreign.ValueLayout.ADDRESS
import java.lang.foreign.ValueLayout.JAVA_INT
import java.lang.invoke.MethodHandles
import java.lang.invoke.MethodType

/**
 * Builds native upcall stubs for the msquic callback shape shared by listeners, connections and
 * streams: `QUIC_STATUS (*)(HQUIC handle, void* Context, EVENT* Event)` — i.e. `int(handle, ctx,
 * event)` with all three carriers as pointers.
 *
 * Every stub is created on [Arena.global] so it lives for the whole JVM (there are only ~3 singleton
 * stubs — no close hazard, per plan). The wrapper GUARANTEES no Kotlin exception ever unwinds across
 * the native boundary: any [Throwable] from the [Handler] becomes `QUIC_STATUS_INTERNAL_ERROR`.
 *
 * Krok 1 only provides the stub factory; the real event dispatch ([Handler] bodies) lands in Kroki
 * 2-3. A [Handler] typically reads [Layouts.eventType] and resolves its object via [CallbackRegistry].
 */
object Upcalls {
    private val linker: Linker = Linker.nativeLinker()

    /** `int(handle, ctx, event)` — the exact descriptor for QUIC_{LISTENER,CONNECTION,STREAM}_CALLBACK. */
    val CALLBACK_DESCRIPTOR: FunctionDescriptor = FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS, ADDRESS)

    /** The Kotlin side of a msquic callback. Return a QUIC_STATUS (`>= 0` = success). */
    fun interface Handler {
        fun handle(handle: MemorySegment, context: MemorySegment, event: MemorySegment): Int
    }

    /**
     * A named JVM method target the [Linker] can bind exactly to [CALLBACK_DESCRIPTOR]'s
     * `(MemorySegment, MemorySegment, MemorySegment)int` MethodType. The try/catch is the boundary
     * guard — never throw into native code.
     */
    class Target(private val handler: Handler) {
        @Suppress("TooGenericExceptionCaught") // deliberate: this IS the native boundary guard —
        // it must catch Throwable (not just Exception), since ANYTHING escaping here unwinds into
        // native msquic code, which is undefined behavior / a JVM crash, not a recoverable failure.
        fun invoke(handle: MemorySegment, context: MemorySegment, event: MemorySegment): Int =
            try {
                handler.handle(handle, context, event)
            } catch (t: Throwable) {
                // Previously silent (SwallowedException): a bug in a Handler vanished with zero
                // diagnostics, surfacing only as an opaque QUIC_STATUS_INTERNAL_ERROR to msquic. Log
                // it — still never rethrows across the native boundary, but the failure is now
                // observable instead of lost.
                System.err.println("[msquic] upcall handler threw, returning QUIC_STATUS_INTERNAL_ERROR: $t")
                Constants.QUIC_STATUS_INTERNAL_ERROR
            }
    }

    private val TARGET_MT: MethodType = MethodType.methodType(
        Int::class.javaPrimitiveType,
        MemorySegment::class.java,
        MemorySegment::class.java,
        MemorySegment::class.java,
    )

    /**
     * Builds a long-lived native function pointer wrapping [handler]. The returned [MemorySegment] is
     * the `void*` you pass as `Handler` to ListenerOpen / ConnectionOpen / StreamOpen /
     * SetCallbackHandler.
     */
    fun stub(handler: Handler): MemorySegment {
        val mh = MethodHandles.lookup()
            .findVirtual(Target::class.java, "invoke", TARGET_MT)
            .bindTo(Target(handler))
        return linker.upcallStub(mh, CALLBACK_DESCRIPTOR, Arena.global())
    }
}
