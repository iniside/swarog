pluginManagement {
    val quarkusPluginVersion: String by settings
    val kotlinVersion: String by settings
    repositories {
        mavenCentral()
        gradlePluginPortal()
    }
    plugins {
        kotlin("jvm") version kotlinVersion
        kotlin("plugin.allopen") version kotlinVersion
        kotlin("plugin.jpa") version kotlinVersion
        id("io.quarkus") version quarkusPluginVersion
    }
}
rootProject.name = "jvm-quarkus-sketch"

// Contracts — plain kotlin("jvm"), no Quarkus: the only surfaces modules share.
include("accounts-events")
include("characters-events")
include("characters-api")
include("admin-api")

// Cross-cutting infra shell (RoleConfig + outbox helper land here in later steps).
include("platform")

// Feature impls — beans.xml-indexed; allopen/jpa where entities/CDI annotations live.
include("accounts")
include("characters")
include("inventory")
include("admin")

// Remote capability adapter: the OUT-OF-PROCESS PlayerCharacters producer (edge/QUIC client). A
// service that does NOT host `characters` depends on THIS (contract + edge only, no characters impl),
// so bean PRESENCE — not RoleConfig — decides local-vs-remote. See characters-client/README intent.
include("characters-client")

// App-shell modules — each applies io.quarkus and produces a SEPARATE fast-jar that links ONLY its own
// modules (real per-service split, mirroring the Go backend's cmd/<svc> entrypoints):
//   app                 = monolith  (all impls, local producers, roles=all)
//   characters-service  = split process A (accounts + characters + local producer + edge QUIC server)
//   inventory-service   = split process B (inventory + admin + characters-client REMOTE producer)
include("app")
include("characters-service")
include("inventory-service")

// Client-edge RPC core — transport-agnostic request/response + server-push over a bidirectional
// byte-stream (the "missing element" QUIC lacks). Schema-less MessagePack via Jackson. A future
// MsQuicTransport : EdgeTransport drops in unchanged. Standalone slice; no feature dependencies.
include("edge")
