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
    implementation("io.quarkus:quarkus-qute")   // HTML templating
    implementation("io.quarkus:quarkus-arc")    // io.quarkus.arc.All (@All List<AdminDataProvider>)

    api(project(":admin-api"))           // injects @All List<AdminDataProvider>; fetches AdminItemDto
    implementation(project(":platform"))       // RoleConfig — local vs remote branch per module
}

allOpen {
    annotation("jakarta.enterprise.context.ApplicationScoped")
    annotation("jakarta.ws.rs.Path")
}
