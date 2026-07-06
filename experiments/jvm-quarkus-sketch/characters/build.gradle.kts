// characters impl — owns schema "characters", the Character @Entity, implements PlayerCharacters,
// emits Created/Deleted, and contributes an admin Item. No io.quarkus plugin (only `app` has it).
plugins {
    kotlin("jvm")
    kotlin("plugin.allopen")
    kotlin("plugin.jpa")
}

val quarkusPlatformGroupId: String by project
val quarkusPlatformArtifactId: String by project
val quarkusPlatformVersion: String by project

dependencies {
    implementation(enforcedPlatform("$quarkusPlatformGroupId:$quarkusPlatformArtifactId:$quarkusPlatformVersion"))
    implementation("io.quarkus:quarkus-kotlin")
    implementation("io.quarkus:quarkus-hibernate-orm-panache-kotlin")
    implementation("io.quarkus:quarkus-jdbc-postgresql")
    implementation("io.quarkus:quarkus-narayana-jta")   // @Transactional domain-write + outbox-append
    implementation("io.quarkus:quarkus-scheduler")       // @Scheduled outbox relay (broker-less HTTP fanout)
    implementation("io.quarkus:quarkus-jackson")         // ObjectMapper bean for payload serialization
    implementation("io.quarkus:quarkus-rest")            // GET /admin-data/characters
    implementation("io.quarkus:quarkus-rest-jackson")    // JSON serialization of AdminItemDto

    api(project(":characters-api"))      // @Produces PlayerCharacters (the produced capability bean)
    api(project(":characters-events"))   // CharacterCreated/Deleted payloads (emitted via outbox relay)
    implementation(project(":edge"))     // edge RPC core + MsQuicServerTransport (ownerOf over QUIC)
    api(project(":admin-api"))           // @Produces Item (public return type)
    implementation(project(":platform"))

    // `characters` had no test source set before this. Pure-unit + module-level tests only (matches
    // the `inventory`/`platform` pattern) — cross-module @QuarkusTest still lives in
    // `app/src/test/kotlin/domain/`, since only app-shells apply `io.quarkus`.
    testImplementation("org.junit.jupiter:junit-jupiter:5.11.0")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

allOpen {
    annotation("jakarta.enterprise.context.ApplicationScoped")
    annotation("jakarta.persistence.Entity")
}
