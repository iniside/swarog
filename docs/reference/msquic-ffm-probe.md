# msquic from the JVM via Panama/FFM — feasibility proven (JDK 26)

**Result:** msquic **v2.5.9** is loadable and callable from **JDK 26** through the Foreign Function & Memory
API — **no JNI, no jextract**. Both the entry point and the api-table calling mechanic work:

```
MsQuicOpenVersion status=0x00000000  apiTable=<native ptr>
RegistrationOpen  status=0x00000000  handle=<native ptr>
PROBE OK: msquic opened, Registration opened+closed, closed — full FFM path via the api table
```

This settles the question "what stops us from using msquic": nothing. Acquisition and the FFM binding pattern
are proven with running code.

## Acquisition (prebuilt, no build-from-source)
msquic does NOT attach binaries to its GitHub Releases. Prebuilt natives ship via **NuGet**:
```
curl -sL -o msquic.nupkg https://www.nuget.org/api/v2/package/Microsoft.Native.Quic.MsQuic.Schannel
# .nupkg is a zip; extract:
#   build/native/bin/x64/msquic.dll   (536 KB, win-x64, schannel = OS TLS)
#   build/native/include/msquic.h     (the C API)
```
(`...MsQuic.OpenSSL` variant exists for a bundled TLS; schannel uses Windows' own TLS — simplest on Windows.)
Linux: `libmsquic` via apt/dnf. macOS: NuGet/OpenSSL variant.

## The FFM pattern (the reusable mechanic)
- Load: `SymbolLookup.libraryLookup(Path.of("…/msquic.dll"), arena)`.
- Entry point: `MsQuicOpen2(QuicApi)` is a macro → real export **`MsQuicOpenVersion(uint32 Version, const void** QuicApi)`**
  returning `QUIC_STATUS` (uint32; 0 = success). `MsQuicClose(apiTable)` frees it (real export).
- **The api table is a struct of ~35 function pointers** (`QUIC_API_TABLE`). To call one: reinterpret the
  returned pointer, read the pointer at `index*8` (x64), and `Linker.downcallHandle` it with the right
  `FunctionDescriptor`. Index map (order in msquic.h v2.5.9):
  `0 SetContext, 1 GetContext, 2 SetCallbackHandler, 3 SetParam, 4 GetParam, 5 RegistrationOpen,
   6 RegistrationClose, 7 RegistrationShutdown, 8 ConfigurationOpen, 9 ConfigurationClose,
   10 ConfigurationLoadCredential, 11 ListenerOpen, 12 ListenerClose, 13 ListenerStart, 14 ListenerStop,
   15 ConnectionOpen, 16 ConnectionClose, 17 ConnectionShutdown, 18 ConnectionStart,
   19 ConnectionSetConfiguration, 20 ConnectionSendResumptionTicket, 21 StreamOpen, 22 StreamClose,
   23 StreamStart, 24 StreamShutdown, 25 StreamSend, 26 StreamReceiveComplete, 27 StreamReceiveSetEnabled,
   28 DatagramSend, 29 ConnectionResumptionTicketValidationComplete, 30 ConnectionCertificateValidationComplete,
   31 ConnectionOpenInPartition (v2.5+)`.
- Run: `java --enable-native-access=ALL-UNNAMED MsQuicProbe.java` (single-file, JDK 26). The probe lives at
  `G:\tmp\msquic\MsQuicProbe.java` (scratch — hardcoded dll path; reference, not part of the build).

## Remaining build for `MsQuicTransport : EdgeTransport` (staged — this is the real work)
The mechanic is proven; the transport is a substantial native-ABI build on top:
1. **Configuration + TLS credential** — `ConfigurationOpen` (ALPN + `QUIC_SETTINGS`) + `ConfigurationLoadCredential`.
   Server needs a real cert (schannel: a self-signed machine cert by thumbprint). This is the fiddliest setup.
2. **Listener** (server) — `ListenerOpen`/`ListenerStart` + a **listener callback** (FFM **upcall**: native→JVM)
   handling NEW_CONNECTION; wire each connection's `SetCallbackHandler`.
3. **Connection callback** (upcall) — CONNECTED / SHUTDOWN / PEER_STREAM_STARTED.
4. **Stream** — `StreamOpen`/`StreamStart`/`StreamSend` + a **stream callback** (upcall) for RECEIVE
   (marshal `QUIC_BUFFER` arrays) / SEND_COMPLETE / SHUTDOWN.
5. Wrap 1–4 behind `EdgeConnection` (send/receive frames) + `EdgeTransport` (accept) so the existing
   `EdgeServer`/`EdgeClient`/`EdgeCodec` (see [[edge-gateway-quic]]) run unchanged over real QUIC.

The hard parts are the **event-callback upcalls** (struct layouts of the `QUIC_*_EVENT` unions) and the
**server TLS credential**. The msquic-Panama binding then becomes the parity option vs Netty-quiche under the
same `EdgeTransport`.
