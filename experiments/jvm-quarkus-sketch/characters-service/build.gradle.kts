// characters-service — split PROCESS A as a SEPARATE fast-jar. Applies io.quarkus (like `app`) and
// aggregates ONLY accounts + characters (their Quarkus extensions ride the runtime classpath
// transitively, so augmentation discovers every extension/entity/bean). It links the LOCAL
// PlayerCharacters producer (via the `characters` impl) and stands up the edge QUIC ownerOf server.
// It has NO inventory/admin/characters-client deps — this jar cannot see process B's code.
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
    implementation("com.fasterxml.jackson.module:jackson-module-kotlin")   // constructor-based data-class deserialization
    implementation("io.quarkus:quarkus-smallrye-health")   // /q/health/ready readiness probe (install.ps1 polls it)

    implementation(project(":accounts"))     // accounts migrations + PlayerRegistered outbox
    implementation(project(":characters"))   // LOCAL PlayerCharacters producer + edge QUIC server + outbox relay
    implementation(project(":platform"))     // RoleConfig
}
