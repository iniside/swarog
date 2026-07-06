// gateway impl — the external QUIC front door. A plain library (NO io.quarkus plugin; only the
// app-shells have it). It recomposes the `edge` stack into a prefix ROUTER: a QUIC EdgeServer whose
// handlers are cached outbound EdgeClients forwarding to the owning service by method-prefix. It links
// ONLY `edge` + `platform` — NEVER a feature impl (characters/inventory/accounts/admin) — because it
// byte-relays: it forwards the request bytes verbatim and never decodes a domain DTO, so it needs no
// domain contract. Bean PRESENCE (this module on a shell's classpath) makes a process a gateway.
plugins {
    kotlin("jvm")
    kotlin("plugin.allopen")   // @ApplicationScoped GatewayEdgeServer must be open (CDI client proxy)
}

val quarkusPlatformGroupId: String by project
val quarkusPlatformArtifactId: String by project
val quarkusPlatformVersion: String by project

dependencies {
    implementation(enforcedPlatform("$quarkusPlatformGroupId:$quarkusPlatformArtifactId:$quarkusPlatformVersion"))
    implementation("io.quarkus:quarkus-kotlin")
    implementation("io.quarkus:quarkus-arc")   // @ApplicationScoped + @Observes StartupEvent + @ConfigProperty
    // @Route (reactive routes; io.quarkus.vertx.web.Route) + the injectable Vertx bean. NOT
    // `quarkus-vertx-http` — that artifact hosts the HTTP server internals but does not carry the
    // `io.quarkus.vertx.web` annotation package; the older `quarkus-vertx-web` artifact id was renamed
    // to `quarkus-reactive-routes` for Quarkus 3.x (verified against the resolved 3.37.1 jars — neither
    // `quarkus-vertx-http` nor its deployment-spi contains an `io/quarkus/vertx/web/` package).
    implementation("io.quarkus:quarkus-reactive-routes")

    // HTTP reverse-proxy side (Step 4). PINNED to 4.5.28 EXACTLY — the Vert.x line Quarkus 3.37.1
    // bundles as its core (`quarkus-vertx-http` -> `quarkus-vertx` -> vertx-core 4.5.x); this artifact
    // is NOT in the Quarkus BOM (the enforcedPlatform above cannot see/override it), and vertx-http-proxy
    // 5.x is built against Vert.x 5 core, which clashes with Quarkus's bundled Vert.x 4.5 core at
    // augmentation time — the exact mismatch class this repo hit before. Do not bump without re-checking
    // the Quarkus 3.37.1 BOM's vertx-core version.
    implementation("io.vertx:vertx-http-proxy:4.5.28")

    implementation(project(":edge"))      // EdgeRouter/EdgeServer/EdgeClient/ForwardFailedException + msquic transports
    implementation(project(":platform"))  // RoleConfig (role-gate the gateway start)

    // Pure JVM unit test over the loopback transport — plain JUnit5, no Quarkus boot.
    testImplementation("org.junit.jupiter:junit-jupiter:5.11.0")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

allOpen {
    annotation("jakarta.enterprise.context.ApplicationScoped")
}

// RoutedBackend eagerly constructs a retained MsQuicClientTransport (native msquic load + FFM upcall
// stubs) even when its `connect` boundary is a loopback test fake and no QUIC is ever dialed — same as
// charactersclient.EdgeRemotePlayerCharacters. Those restricted native-access calls throw on JDK 22+
// unless the module is granted access, so the routing unit test needs this flag. The vendored msquic.dll
// rides transitively from the `:edge` dependency's resources; no extra dep needed.
tasks.test {
    jvmArgs("--enable-native-access=ALL-UNNAMED")
    // Forward the LivePlayerClientSmokeTest coordinates to the forked test JVM. Unset => the live smoke
    // self-skips (Assumptions.assumeTrue on gateway.smoke.host), so the normal `test` sweep is unaffected;
    // the Step-6 driver passes them (-Dgateway.smoke.host=localhost -Dgateway.smoke.playerId=... etc.)
    // against a running install.ps1 -Mode microservices split.
    listOf(
        "gateway.smoke.host",
        "gateway.smoke.port",
        "gateway.smoke.playerId",
        "gateway.smoke.characterName",
        "gateway.smoke.characterId",
        "gateway.smoke.charactersDown",
    ).forEach { key -> System.getProperty(key)?.let { systemProperty(key, it) } }
}
