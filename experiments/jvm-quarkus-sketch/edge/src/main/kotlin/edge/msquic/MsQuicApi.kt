package edge.msquic

import java.lang.foreign.FunctionDescriptor
import java.lang.foreign.Linker
import java.lang.foreign.MemorySegment
import java.lang.foreign.ValueLayout.ADDRESS
import java.lang.foreign.ValueLayout.JAVA_INT
import java.lang.foreign.ValueLayout.JAVA_LONG
import java.lang.foreign.ValueLayout.JAVA_SHORT
import java.lang.invoke.MethodHandle

/**
 * Typed downcall wrappers over the `QUIC_API_TABLE` function-pointer table returned by
 * `MsQuicOpenVersion`. Each function pointer is read from `apiTable.get(ADDRESS, index * 8)` (x64:
 * 8-byte pointers) at the index of its field in `struct QUIC_API_TABLE` (msquic.h:1787), then wrapped
 * in a [Linker.downcallHandle] with the matching [FunctionDescriptor].
 *
 * Verified indices (counted field-by-field against the header, v2.5.9):
 * ```
 *   0 SetContext              7 RegistrationShutdown   14 ListenerStop            21 StreamOpen
 *   1 GetContext             8 ConfigurationOpen      15 ConnectionOpen          22 StreamClose
 *   2 SetCallbackHandler     9 ConfigurationClose     16 ConnectionClose         23 StreamStart
 *   3 SetParam              10 ConfigurationLoadCred  17 ConnectionShutdown      24 StreamShutdown
 *   4 GetParam              11 ListenerOpen           18 ConnectionStart         25 StreamSend
 *   5 RegistrationOpen      12 ListenerClose          19 ConnectionSetConfig     26 StreamReceiveComplete
 *   6 RegistrationClose     13 ListenerStart          20 ConnectionSendResumpt   27 StreamReceiveSetEnabled
 * ```
 *
 * ABI mapping: every HQUIC/pointer arg = `ADDRESS`, `QUIC_STATUS`/enum/flags = `JAVA_INT`,
 * uint16 (port, family) = `JAVA_SHORT`, `QUIC_UINT62` error code = `JAVA_LONG`.
 *
 * Calls use [MethodHandle.invoke] (lenient asType) rather than `invokeExact` — these are low-frequency
 * control-plane calls, and `invoke` avoids Kotlin's signature-polymorphic pitfalls while preserving
 * the exact carrier types.
 */
@Suppress("TooManyFunctions") // one method per QUIC_API_TABLE entry this binding uses — the function
// count mirrors the native C API surface it wraps 1:1; splitting it would fragment one cohesive ABI
// table into arbitrary pieces with no natural seam.
class MsQuicApi(private val linker: Linker, private val apiTable: MemorySegment) {

    private fun handleAt(index: Int, descriptor: FunctionDescriptor): MethodHandle {
        val fnPtr = apiTable.get(ADDRESS, index.toLong() * 8)
        require(fnPtr.address() != 0L) { "QUIC_API_TABLE[$index] is null" }
        return linker.downcallHandle(fnPtr, descriptor)
    }

    // index 2 — void SetCallbackHandler(HQUIC, void* handler, void* ctx)
    private val setCallbackHandlerH =
        handleAt(2, FunctionDescriptor.ofVoid(ADDRESS, ADDRESS, ADDRESS))

    // index 5 — QUIC_STATUS RegistrationOpen(const QUIC_REGISTRATION_CONFIG*, HQUIC* out)
    private val registrationOpenH =
        handleAt(5, FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS))

    // index 6 — void RegistrationClose(HQUIC)
    private val registrationCloseH = handleAt(6, FunctionDescriptor.ofVoid(ADDRESS))

    // index 7 — void RegistrationShutdown(HQUIC, QUIC_CONNECTION_SHUTDOWN_FLAGS, QUIC_UINT62 err)
    private val registrationShutdownH =
        handleAt(7, FunctionDescriptor.ofVoid(ADDRESS, JAVA_INT, JAVA_LONG))

    // index 8 — QUIC_STATUS ConfigurationOpen(HQUIC reg, const QUIC_BUFFER* alpn, uint32 alpnCount,
    //           const QUIC_SETTINGS* settings, uint32 settingsSize, void* ctx, HQUIC* out)
    private val configurationOpenH = handleAt(
        8, FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS, JAVA_INT, ADDRESS, JAVA_INT, ADDRESS, ADDRESS),
    )

    // index 9 — void ConfigurationClose(HQUIC)
    private val configurationCloseH = handleAt(9, FunctionDescriptor.ofVoid(ADDRESS))

    // index 10 — QUIC_STATUS ConfigurationLoadCredential(HQUIC cfg, const QUIC_CREDENTIAL_CONFIG*)
    private val configurationLoadCredentialH =
        handleAt(10, FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS))

    // index 11 — QUIC_STATUS ListenerOpen(HQUIC reg, handler, void* ctx, HQUIC* out)
    private val listenerOpenH =
        handleAt(11, FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS, ADDRESS, ADDRESS))

    // index 12 — void ListenerClose(HQUIC)
    private val listenerCloseH = handleAt(12, FunctionDescriptor.ofVoid(ADDRESS))

    // index 13 — QUIC_STATUS ListenerStart(HQUIC listener, const QUIC_BUFFER* alpn, uint32 alpnCount,
    //            const QUIC_ADDR* localAddr)
    private val listenerStartH =
        handleAt(13, FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS, JAVA_INT, ADDRESS))

    // index 14 — void ListenerStop(HQUIC)
    private val listenerStopH = handleAt(14, FunctionDescriptor.ofVoid(ADDRESS))

    // index 15 — QUIC_STATUS ConnectionOpen(HQUIC reg, handler, void* ctx, HQUIC* out)
    private val connectionOpenH =
        handleAt(15, FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS, ADDRESS, ADDRESS))

    // index 16 — void ConnectionClose(HQUIC)
    private val connectionCloseH = handleAt(16, FunctionDescriptor.ofVoid(ADDRESS))

    // index 17 — void ConnectionShutdown(HQUIC, QUIC_CONNECTION_SHUTDOWN_FLAGS, QUIC_UINT62 err)
    private val connectionShutdownH =
        handleAt(17, FunctionDescriptor.ofVoid(ADDRESS, JAVA_INT, JAVA_LONG))

    // index 18 — QUIC_STATUS ConnectionStart(HQUIC conn, HQUIC cfg, QUIC_ADDRESS_FAMILY family,
    //            const char* serverName, uint16 port)
    private val connectionStartH =
        handleAt(18, FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS, JAVA_SHORT, ADDRESS, JAVA_SHORT))

    // index 19 — QUIC_STATUS ConnectionSetConfiguration(HQUIC conn, HQUIC cfg)
    private val connectionSetConfigurationH =
        handleAt(19, FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS))

    // index 21 — QUIC_STATUS StreamOpen(HQUIC conn, QUIC_STREAM_OPEN_FLAGS, handler, void* ctx, HQUIC* out)
    private val streamOpenH =
        handleAt(21, FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_INT, ADDRESS, ADDRESS, ADDRESS))

    // index 22 — void StreamClose(HQUIC)
    private val streamCloseH = handleAt(22, FunctionDescriptor.ofVoid(ADDRESS))

    // index 23 — QUIC_STATUS StreamStart(HQUIC stream, QUIC_STREAM_START_FLAGS)
    private val streamStartH = handleAt(23, FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_INT))

    // index 24 — void StreamShutdown(HQUIC, QUIC_STREAM_SHUTDOWN_FLAGS, QUIC_UINT62 err)
    private val streamShutdownH =
        handleAt(24, FunctionDescriptor.ofVoid(ADDRESS, JAVA_INT, JAVA_LONG))

    // index 25 — QUIC_STATUS StreamSend(HQUIC stream, const QUIC_BUFFER* bufs, uint32 bufCount,
    //            QUIC_SEND_FLAGS, void* clientCtx)
    private val streamSendH =
        handleAt(25, FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS, JAVA_INT, JAVA_INT, ADDRESS))

    // ---- Typed wrappers --------------------------------------------------------------------------

    fun setCallbackHandler(handle: MemorySegment, handler: MemorySegment, context: MemorySegment) {
        setCallbackHandlerH.invoke(handle, handler, context)
    }

    fun registrationOpen(config: MemorySegment, out: MemorySegment): Int =
        registrationOpenH.invoke(config, out) as Int

    fun registrationClose(registration: MemorySegment) {
        registrationCloseH.invoke(registration)
    }

    /**
     * Initiates shutdown of ALL connections owned by this registration, operating on the (still-valid)
     * registration handle rather than per-connection handles — so it is safe to call before
     * [registrationClose] even while connection callbacks are concurrently freeing individual handles.
     * This makes the subsequent (blocking) [registrationClose] drain quickly instead of waiting on the
     * idle timeout.
     */
    fun registrationShutdown(registration: MemorySegment, flags: Int, errorCode: Long) {
        registrationShutdownH.invoke(registration, flags, errorCode)
    }

    @Suppress("LongParameterList") // mirrors msquic's native `ConfigurationOpen(HQUIC, const QUIC_BUFFER*,
    // uint32, const QUIC_SETTINGS*, uint32, void*, HQUIC*)` 1:1 — the parameter list IS the ABI, not a
    // design choice to bundle into a parameter object (that would just move the same 7 fields elsewhere).
    fun configurationOpen(
        registration: MemorySegment,
        alpnBuffers: MemorySegment,
        alpnCount: Int,
        settings: MemorySegment,
        settingsSize: Int,
        context: MemorySegment,
        out: MemorySegment,
    ): Int = configurationOpenH.invoke(
        registration, alpnBuffers, alpnCount, settings, settingsSize, context, out,
    ) as Int

    fun configurationClose(configuration: MemorySegment) {
        configurationCloseH.invoke(configuration)
    }

    fun configurationLoadCredential(configuration: MemorySegment, credConfig: MemorySegment): Int =
        configurationLoadCredentialH.invoke(configuration, credConfig) as Int

    fun listenerOpen(
        registration: MemorySegment,
        handler: MemorySegment,
        context: MemorySegment,
        out: MemorySegment,
    ): Int = listenerOpenH.invoke(registration, handler, context, out) as Int

    fun listenerClose(listener: MemorySegment) {
        listenerCloseH.invoke(listener)
    }

    fun listenerStart(
        listener: MemorySegment,
        alpnBuffers: MemorySegment,
        alpnCount: Int,
        localAddress: MemorySegment,
    ): Int = listenerStartH.invoke(listener, alpnBuffers, alpnCount, localAddress) as Int

    fun listenerStop(listener: MemorySegment) {
        listenerStopH.invoke(listener)
    }

    fun connectionOpen(
        registration: MemorySegment,
        handler: MemorySegment,
        context: MemorySegment,
        out: MemorySegment,
    ): Int = connectionOpenH.invoke(registration, handler, context, out) as Int

    fun connectionClose(connection: MemorySegment) {
        connectionCloseH.invoke(connection)
    }

    fun connectionShutdown(connection: MemorySegment, flags: Int, errorCode: Long) {
        connectionShutdownH.invoke(connection, flags, errorCode)
    }

    fun connectionStart(
        connection: MemorySegment,
        configuration: MemorySegment,
        family: Short,
        serverName: MemorySegment,
        port: Short,
    ): Int = connectionStartH.invoke(connection, configuration, family, serverName, port) as Int

    fun connectionSetConfiguration(connection: MemorySegment, configuration: MemorySegment): Int =
        connectionSetConfigurationH.invoke(connection, configuration) as Int

    fun streamOpen(
        connection: MemorySegment,
        openFlags: Int,
        handler: MemorySegment,
        context: MemorySegment,
        out: MemorySegment,
    ): Int = streamOpenH.invoke(connection, openFlags, handler, context, out) as Int

    fun streamClose(stream: MemorySegment) {
        streamCloseH.invoke(stream)
    }

    fun streamStart(stream: MemorySegment, startFlags: Int): Int =
        streamStartH.invoke(stream, startFlags) as Int

    fun streamShutdown(stream: MemorySegment, flags: Int, errorCode: Long) {
        streamShutdownH.invoke(stream, flags, errorCode)
    }

    fun streamSend(
        stream: MemorySegment,
        buffers: MemorySegment,
        bufferCount: Int,
        sendFlags: Int,
        clientContext: MemorySegment,
    ): Int = streamSendH.invoke(stream, buffers, bufferCount, sendFlags, clientContext) as Int
}
