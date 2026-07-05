// Client-edge RPC core — a tiny, transport-agnostic request/response + server-push protocol over a
// bidirectional byte-stream (the application-level layer QUIC itself lacks). Serialization is
// schema-less MessagePack via Jackson (the SAME data-class reflection the rest of the project uses —
// NO protobuf, NO codegen). No io.quarkus plugin (only `app` has it); it's a plain library slice.
// beans.xml + allopen are carried so a later step can expose EdgeServer as a CDI bean, but THIS
// increment is a pure JVM unit test — no Quarkus runtime is booted.
plugins {
    kotlin("jvm")
    kotlin("plugin.allopen")
}

val quarkusPlatformGroupId: String by project
val quarkusPlatformArtifactId: String by project
val quarkusPlatformVersion: String by project

dependencies {
    implementation(enforcedPlatform("$quarkusPlatformGroupId:$quarkusPlatformArtifactId:$quarkusPlatformVersion"))
    implementation("io.quarkus:quarkus-kotlin")
    implementation("io.quarkus:quarkus-arc")     // @ApplicationScoped surface for a future EdgeServer bean

    // The MessagePack ObjectMapper: MessagePackFactory emits binary MessagePack from ordinary Kotlin
    // data classes (reflection, no schema). jackson-module-kotlin makes constructor-based data classes
    // DESERIALIZE (quarkus-jackson does not pull it in). BOM manages the jackson-* versions.
    implementation("io.quarkus:quarkus-jackson")
    implementation("com.fasterxml.jackson.module:jackson-module-kotlin")
    implementation("org.msgpack:jackson-dataformat-msgpack:0.9.8")

    implementation(project(":platform"))   // shared infra slice (RoleConfig etc.) — the only feature dep

    // Pure JVM unit test over the loopback transport — plain JUnit5, no Quarkus boot.
    testImplementation("org.junit.jupiter:junit-jupiter:5.11.0")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

allOpen {
    annotation("jakarta.enterprise.context.ApplicationScoped")
}

// The msquic FFM layer (edge.msquic) uses restricted native-access APIs (SymbolLookup.libraryLookup,
// Linker downcall/upcall). JDK 22+ requires the module to be granted native access explicitly, or the
// restricted calls throw at runtime. The unit tests exercise the real msquic.dll, so grant it here.
tasks.test {
    jvmArgs("--enable-native-access=ALL-UNNAMED")
}
