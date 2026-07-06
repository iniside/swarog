// gateway-service — the external front-door PROCESS as a SEPARATE fast-jar. Applies io.quarkus (like
// the other shells) and links ONLY the `gateway` router + `edge` transport + `platform` — NO feature
// impls (characters/inventory/accounts/admin), NO Stork. It is a pure QUIC prefix router: it byte-relays
// player calls to the owning service by method-prefix, so it needs no domain code on its classpath. The
// composition-rule gate in the root build asserts that (forbids the feature impls transitively).
plugins {
    kotlin("jvm")
    id("io.quarkus")
}

val quarkusPlatformGroupId: String by project
val quarkusPlatformArtifactId: String by project
val quarkusPlatformVersion: String by project

dependencies {
    implementation(enforcedPlatform("$quarkusPlatformGroupId:$quarkusPlatformArtifactId:$quarkusPlatformVersion"))
    implementation("io.quarkus:quarkus-kotlin")
    implementation("io.quarkus:quarkus-arc")
    implementation("io.quarkus:quarkus-jackson")
    implementation("com.fasterxml.jackson.module:jackson-module-kotlin")   // edge codec's Kotlin data-class serde
    implementation("io.quarkus:quarkus-smallrye-health")   // /q/health/ready readiness probe (install.ps1 polls it)

    implementation(project(":gateway"))    // GatewayEdgeServer: the QUIC prefix router
    implementation(project(":edge"))       // edge RPC core + msquic transports (the router's substrate)
    implementation(project(":platform"))   // RoleConfig
    implementation(project(":arch-rules"))  // Layer 3 (opt-in demo): build-time architecture-validation extension
    // NOTE: no :characters/:inventory/:accounts/:admin — proven by the composition rule in root build.
}
