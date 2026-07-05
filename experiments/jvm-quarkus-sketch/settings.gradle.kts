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
include("characters-grpc")   // proto + generated gRPC/Mutiny stubs (shared by characters server + inventory client)
include("admin-api")

// Cross-cutting infra shell (RoleConfig + outbox helper land here in later steps).
include("platform")

// Feature impls — beans.xml-indexed; allopen/jpa where entities/CDI annotations live.
include("accounts")
include("characters")
include("inventory")
include("admin")

// The ONLY io.quarkus module: aggregates impls, holds the seed + application.properties,
// runs quarkusBuild.
include("app")

// Client-edge RPC core — transport-agnostic request/response + server-push over a bidirectional
// byte-stream (the "missing element" QUIC lacks). Schema-less MessagePack via Jackson. A future
// MsQuicTransport : EdgeTransport drops in unchanged. Standalone slice; no feature dependencies.
include("edge")
