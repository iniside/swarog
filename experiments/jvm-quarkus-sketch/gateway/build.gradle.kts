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
}
