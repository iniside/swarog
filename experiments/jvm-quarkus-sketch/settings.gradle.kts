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
        id("io.quarkus.extension") version quarkusPluginVersion
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

// Verification Layer 3 (OPT-IN demo) — a Quarkus build-time extension that re-implements Layer-1's
// architecture checks against ArC's AUGMENTED bean graph + Jandex, failing `quarkusBuild`. Two modules:
//   arch-rules            = the (empty) RUNTIME; its generated quarkus-extension.properties descriptor is
//                           what makes discovery flow — app-shells put THIS on their classpath.
//   arch-rules-deployment = the DEPLOYMENT half (JAVA sources only — quarkus-extension-processor does NOT
//                           index Kotlin @BuildStep classes, #35110, so the build-steps.list would be empty
//                           and validation would SILENTLY never run).
// The project NAME must EXACTLY equal the `deploymentModule.set(...)` string in arch-rules/runtime — the
// extension plugin resolves the deployment half by local project name (ToolingUtils.findLocalProject), so a
// mismatch is a silent no-op.
include("arch-rules")
project(":arch-rules").projectDir = file("arch-rules/runtime")
include("arch-rules-deployment")
project(":arch-rules-deployment").projectDir = file("arch-rules/deployment")

// Client-edge RPC core — transport-agnostic request/response + server-push over a bidirectional
// byte-stream (the "missing element" QUIC lacks). Schema-less MessagePack via Jackson. A future
// MsQuicTransport : EdgeTransport drops in unchanged. Standalone slice; no feature dependencies.
include("edge")
