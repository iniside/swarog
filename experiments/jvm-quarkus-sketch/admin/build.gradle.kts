// admin impl — the @Path("/admin") resource, Qute template (templates/admin.html) and static
// theme.css live here. No @Entity, so no jpa plugin; allopen only for @Path/@ApplicationScoped.
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
    implementation("io.quarkus:quarkus-rest")                  // JAX-RS endpoints (the /admin resource)
    implementation("io.quarkus:quarkus-rest-client-jackson")   // REST fan-out to remote /admin-data/<id>
    implementation("io.quarkus:quarkus-smallrye-stork")        // resolve stork://<id>-service in the REST fan-out
    implementation("io.smallrye.stork:stork-service-discovery-static-list") // static address-list discovery (BOM-managed version)
    implementation("io.quarkus:quarkus-qute")   // HTML templating
    implementation("io.quarkus:quarkus-arc")    // io.quarkus.arc.All (@All List<AdminDataProvider>)

    api(project(":admin-api"))           // injects @All List<AdminDataProvider>; fetches AdminItemDto
    implementation(project(":platform"))       // RoleConfig — local vs remote branch per module

    // `admin` had no test source set before this. Deps mirror `app`'s @QuarkusTest set (BOM-managed
    // versions, no explicit version numbers) so pure-unit/CDI-substitution tests can be written here
    // in a later step. NOTE: `admin` does NOT apply `io.quarkus` (only app-shells do, per
    // settings.gradle.kts) — @QuarkusTest needs the augmented application model that only an
    // io.quarkus-applying module produces, so any CROSS-module `@QuarkusTest` against the wired
    // admin resource still belongs in `app/src/test/kotlin/domain/`, not here. These deps let admin
    // host pure-unit tests (e.g. the Basic-auth pure fn, the slug-extraction golden-master) and are
    // future-proofing if an admin-local @QuarkusTest turns out to work; unproven either way yet.
    testImplementation("io.quarkus:quarkus-junit5")
    testImplementation("io.rest-assured:rest-assured")
    testImplementation("io.quarkus:quarkus-junit5-mockito")
    testImplementation("org.junit.jupiter:junit-jupiter:5.11.0")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

allOpen {
    annotation("jakarta.enterprise.context.ApplicationScoped")
    annotation("jakarta.ws.rs.Path")
}
