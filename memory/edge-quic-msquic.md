---
name: edge-quic-msquic
description: GameBackend player-edge transport is QUIC via msquic bound with JDK 26 Panama/FFM; proven by replacing the internal gRPC ownerOf seam
metadata: 
  node_type: memory
  type: project
  originSessionId: 2dde7081-732d-49f5-b0aa-ce19637ba5f1
---

The client-edge transport (player↔backend) is **QUIC**, and internally it's **msquic bound into the JVM via
JDK 26 Panama/FFM** (no JNI, no jextract, no Netty). Decided and proven in `experiments/jvm-quarkus-sketch/edge`.

- **Layering (keep separate):** serialization = **schema-less MessagePack** (Jackson reflection over data
  classes — NO protobuf/FlatBuffers/IDL/codegen); RPC framing = ours (`edge` core: `cid` request/response +
  server-push, one persistent bidi stream, 4-byte-BE length prefix); transport = **QUIC (msquic)**. Transport is
  orthogonal — swappable under the `EdgeTransport`/`EdgeConnection` seam.
- **msquic acquisition:** prebuilt via **NuGet** `Microsoft.Native.Quic.MsQuic.Schannel` (NOT GitHub releases);
  `.nupkg` is a zip → `build/native/bin/x64/msquic.dll` + `msquic.h`. Vendored at
  `edge/src/main/resources/native/msquic.dll`, extracted to temp + `SymbolLookup.libraryLookup` at runtime.
- **Design rule (why QUIC only at the edge):** async = fanout only (broker-less HTTP, see
  [[async-fanout-sync-grpc-brokerless]]); anything needing log/order/buffering = sync. QUIC's wins (0-RTT,
  connection migration, one multiplexed stream for req/resp + push) are for the **player↔backend** hop, not
  internal LAN. The internal `ownerOf` seam was the dogfood that proved the stack — gRPC deleted.
- **TLS = schannel needs a store cert** (PEM/`CERTIFICATE_FILE` is OpenSSL-only): `New-SelfSignedCertificate` in
  `cert:\CurrentUser\My`, feed the 20-byte thumbprint as `QUIC_CERTIFICATE_HASH`. `scripts/ensure-cert.ps1`
  provisions it; same-user only (CurrentUser store, not a service account). Run with
  `--enable-native-access=ALL-UNNAMED`.
- **Hard-won gotchas:** ABI struct offsets/union layouts and api-table indices are load-bearing (a wrong one =
  segfault, invisible to the compiler) — verify against `msquic.h`/`msquic_winuser.h`, assert `byteSize()`.
  Client must dial the server's IP family (localhost→::1 vs 127.0.0.1 IPv4 bind mismatch = handshake fails with a
  misleading ALPN error). Clean shutdown = `RegistrationShutdown` before the blocking `RegistrationClose` (NOT
  per-connection force-close — use-after-free against the async SHUTDOWN_COMPLETE path). RECEIVE bytes valid only
  during the callback (copy immediately); send buffers pinned until SEND_COMPLETE. Handlers run on an EdgeServer
  worker thread with no ambient tx/CDI — wrap Panache reads in `QuarkusTransaction.requiringNew()`.

Full ABI facts: repo `docs/reference/msquic-ffm-probe.md` + `edge-gateway-quic.md`. Next: Increment C = per-engine
client shim (UE C++ / Unity C#) — msgpack + QUIC client + the same cid loop, the actual player-edge use.
