// accounts impl — owns schema "accounts", the Player @Entity, and emits PlayerRegistered.
// Compiles against Quarkus via the BOM but carries NO io.quarkus plugin (only `app` does).
plugins {
    kotlin("jvm")
    kotlin("plugin.allopen")
    kotlin("plugin.jpa")   // no-arg ctor for the @Entity — Hibernate instantiates reflectively
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

    api(project(":accounts-events"))   // PlayerRegistered appears in this module's public API
    implementation(project(":platform"))

    // `accounts` had no test source set before this. Pure-unit + module-level tests only (matches
    // the `inventory`/`platform` pattern) — cross-module @QuarkusTest still lives in
    // `app/src/test/kotlin/domain/`, since only app-shells apply `io.quarkus`.
    testImplementation("org.junit.jupiter:junit-jupiter:5.11.0")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

// CDI normal-scoped beans get client proxies; Hibernate proxies need open entities.
allOpen {
    annotation("jakarta.enterprise.context.ApplicationScoped")
    annotation("jakarta.persistence.Entity")
}
