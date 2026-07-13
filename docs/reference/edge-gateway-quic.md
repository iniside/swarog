> **ARCHIVED (JVM/Quarkus-era)** — describes the retired
> `experiments/jvm-quarkus-sketch/edge` gateway (MessagePack/msquic FFM), not
> the current Rust gateway. Current state: CLAUDE.md.

# Edge gateway (player ↔ backend) — QUIC transport, MessagePack, schema-less

Reference for the client-edge gateway explored in `experiments/jvm-quarkus-sketch/edge`. Distinct from the
internal service-to-service seams (characters↔inventory), which stay on plain HTTP/REST — QUIC buys nothing on
low-loss LAN. The gateway is the **player↔backend** hop, where QUIC's wins are real.

## Why QUIC (at the edge, not internally)
One multiplexed connection carries request/response AND server-push (no bolt-on WebSocket); 0-RTT reconnect;
**connection migration** (player switches wifi↔LTE without dropping the session). These matter for a game
client on a lossy/mobile network — not for an internal RPC over localhost/LAN.

## Layering (the decision)
QUIC is only a **transport** — streams of bytes, no application semantics. So the gateway is three orthogonal
layers; only the transport is QUIC:

1. **Serialization = MessagePack, schema-less, reflection-driven.** No protobuf, no FlatBuffers, no IDL, no
   codegen. On the JVM we reuse Jackson (`jackson-dataformat-msgpack`) over the *same* data classes the project
   already uses. On Unreal: reflect the UStruct → msgpack. Additive evolution comes free (map keys on the wire +
   ignore-unknown, matching CLAUDE.md rule #6). Tradeoff: no compile-time client↔server contract — agreement by
   convention + tests; a schema *document* can be added later without changing the wire format.
   - *Why not protobuf/FlatBuffers:* both are schema + codegen ("machinery"); FlatBuffers' only edge is zero-copy
     reads of large buffers, irrelevant for tiny meta-backend req/resp objects, and its API is heavier.
2. **RPC/framing = ours (the piece no library gives you).** Request/Response correlation (`cid`) + server-push
   over one bidirectional stream. This is `edge`'s core, transport-agnostic.
3. **Transport = QUIC.** On the JVM: native QUIC via bindings — either Netty's incubator QUIC codec (wraps
   Cloudflare *quiche*, off-the-shelf, what Quarkus/Vert.x HTTP/3 uses) OR **msquic via Project Panama / FFM +
   `jextract`** on JDK 26 (no JNI; msquic is C, MS-maintained, used on Windows/Xbox → client+server impl parity).
   msquic costs native builds/glue per server platform; quiche is turnkey but Rust. The transport is orthogonal
   to layers 1–2, so it can be swapped later under an unchanged framing.

## Status
- **Increment A — DONE (commit 529652a), verified.** `edge` module: `EdgeMessage` (Request/Response/Push, `cid`),
  `EdgeCodec` (MessagePack via Jackson; typed payloads are nested msgpack blobs; `typedHandler<Req,Resp>`),
  `EdgeRouter` (method→handler, throws/unknown → error response), `EdgeServer` + `EdgeTransport`/`EdgeConnection`
  interfaces, `LoopbackTransport` + `EdgeClient`. 4 tests green over real in-JVM round-trips (req/resp with cid
  match, handler error, unknown method, server push). Transport-agnostic — QUIC drops in as an `EdgeTransport`.
- **Increment B — DONE, verified over REAL QUIC.** Chose **msquic via Panama/FFM** (not Netty/quiche) —
  feasibility proven then built out. `edge/src/main/kotlin/edge/msquic/`: `MsQuicLibrary` (load vendored dll),
  `MsQuicApi` (api-table downcalls), `Layouts`/`Constants` (verified ABI), `CallbackRegistry`/`Upcalls`,
  `FrameReassembler`, `MsQuicConnection` (EdgeConnection), `MsQuicServerTransport`/`MsQuicClientTransport`
  (EdgeTransport). One persistent bidi stream/connection, 4-byte-BE length framing, schannel cert
  (`CERTIFICATE_HASH`). `MsQuicEchoTest` proves a live QUIC/UDP round-trip; the `edge` core is unchanged.
  ABI/FFM specifics recorded in [[msquic-ffm-probe]]. Plan: `docs/plans/2026-07-05-1520-quic-msquic-migration-plan.md`.
- **gRPC REPLACED (real dogfood).** The internal `PlayerCharacters.ownerOf` seam (inventory→characters) now
  runs over this edge/QUIC transport; the whole gRPC stack (`characters-grpc` module, proto, `@GrpcService`,
  quarkus-grpc) is deleted. Verified in the split: `ownerOf` over QUIC authorizes a real character (200) /
  refuses a nonexistent one (400); monolith takes the in-process local branch (no QUIC/cert). Two runtime-only
  bugs found & fixed during verification: `Optional<String>` cert-thumbprint injection (bean instantiated in
  every process), and `QuarkusTransaction.requiringNew()` around the handler's Panache read (runs on the
  EdgeServer worker thread, no ambient tx/CDI context). msquic clean-shutdown = `RegistrationShutdown` before the
  blocking `RegistrationClose` (per-connection force-close use-after-frees against the async SHUTDOWN_COMPLETE).
- **Increment C — next: client shim** per engine (UE C++ / Unity C#): msgpack (reflection) + a QUIC client +
  the same `cid` request/response + push loop. Written once. (This is the actual player-edge use; the ownerOf
  seam was the internal dogfood proving the stack.)

See also memory [[async-fanout-sync-grpc-brokerless]] (internal async comms are broker-less HTTP; the sync
ownerOf seam is now edge/QUIC) and [[msquic-ffm-probe]] (the FFM binding facts).
