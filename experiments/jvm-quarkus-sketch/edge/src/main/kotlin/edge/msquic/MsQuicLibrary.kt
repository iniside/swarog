package edge.msquic

import java.lang.foreign.Arena
import java.lang.foreign.FunctionDescriptor
import java.lang.foreign.Linker
import java.lang.foreign.MemorySegment
import java.lang.foreign.SymbolLookup
import java.lang.foreign.ValueLayout.ADDRESS
import java.lang.foreign.ValueLayout.JAVA_INT
import java.lang.invoke.MethodHandle
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption

/**
 * Loads the vendored `msquic.dll` and bootstraps the FFM entry point.
 *
 * The dll is shipped as a classpath resource (`/native/msquic.dll`), extracted to a temp file at init
 * (a [SymbolLookup] needs a real filesystem path), then opened with [SymbolLookup.libraryLookup] on
 * [Arena.global] so the library — and everything derived from it (the api table, downcall handles) —
 * lives for the whole JVM.
 *
 * We call `MsQuicOpenVersion(2, &apiTable)` — the REAL exported symbol. (`MsQuicOpen2` in the header
 * is an inline macro over it, NOT an export, so it cannot be looked up.) The returned pointer is a
 * `const QUIC_API_TABLE*`; [api] wraps it as typed downcalls. [close] calls `MsQuicClose(apiTable)`.
 *
 * Requires `--enable-native-access=ALL-UNNAMED` (libraryLookup/downcallHandle are restricted).
 */
class MsQuicLibrary : AutoCloseable {

    private val linker: Linker = Linker.nativeLinker()
    private val closeHandle: MethodHandle

    /** The reinterpreted `QUIC_API_TABLE*` (address preserved; sized so all indices are readable). */
    val apiTable: MemorySegment

    /** Typed downcall surface over [apiTable]. */
    val api: MsQuicApi

    init {
        // Guard the ABI before we touch native memory — cheapest possible catch for an offset drift.
        Layouts.assertSizes()

        val dllPath = extractDll()
        val lookup = SymbolLookup.libraryLookup(dllPath.toAbsolutePath().toString(), Arena.global())

        val openHandle = linker.downcallHandle(
            lookup.find("MsQuicOpenVersion")
                .orElseThrow { IllegalStateException("MsQuicOpenVersion not exported by msquic.dll") },
            FunctionDescriptor.of(JAVA_INT, JAVA_INT, ADDRESS),
        )
        closeHandle = linker.downcallHandle(
            lookup.find("MsQuicClose")
                .orElseThrow { IllegalStateException("MsQuicClose not exported by msquic.dll") },
            FunctionDescriptor.ofVoid(ADDRESS),
        )

        // The out-pointer only needs to survive the call, so a confined arena is enough.
        val tablePtr = Arena.ofConfined().use { tmp ->
            val apiOut = tmp.allocate(ADDRESS) // const QUIC_API_TABLE**
            val status = openHandle.invoke(Constants.QUIC_API_VERSION_2, apiOut) as Int
            check(Constants.succeeded(status)) {
                "MsQuicOpenVersion(2) failed: 0x%08x".format(status)
            }
            val ptr = apiOut.get(ADDRESS, 0)
            check(ptr.address() != 0L) { "MsQuicOpenVersion returned a null api table" }
            ptr
        }

        // The api table pointer comes back zero-length; reinterpret so all ~28 indices are readable.
        apiTable = tablePtr.reinterpret(API_TABLE_BYTES)
        api = MsQuicApi(linker, apiTable)
    }

    /** Releases the msquic library handle. Must be called after all reg/config/conn handles closed. */
    override fun close() {
        closeHandle.invoke(apiTable)
    }

    private fun extractDll(): Path {
        val stream = MsQuicLibrary::class.java.getResourceAsStream(DLL_RESOURCE)
            ?: error("msquic.dll not found on classpath at $DLL_RESOURCE")
        val temp = Files.createTempFile("msquic", ".dll")
        temp.toFile().deleteOnExit()
        stream.use { input ->
            Files.copy(input, temp, StandardCopyOption.REPLACE_EXISTING)
        }
        return temp
    }

    private companion object {
        const val DLL_RESOURCE = "/native/msquic.dll"
        // 48 pointers * 8 bytes covers the full QUIC_API_TABLE (v2.5.9 has ~28 entries) with headroom.
        const val API_TABLE_BYTES = 48L * 8
    }
}
