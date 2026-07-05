// inventory-service — split PROCESS B as a SEPARATE fast-jar. Applies io.quarkus (like `app`) and
// aggregates inventory + admin + the REMOTE characters-client producer. It links the `characters-client`
// module (edge/QUIC PlayerCharacters producer) — NOT the `characters` impl — so the ownerOf capability
// is satisfied over QUIC, and this jar CANNOT see process A's characters/accounts implementation.
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
    // Jackson Kotlin module: the inventory event-sink REST endpoint binds JSON -> CharacterCreated
    // (a constructor-based data class with no no-arg ctor), which fails without it. quarkus-jackson
    // does NOT pull it in.
    implementation("com.fasterxml.jackson.module:jackson-module-kotlin")
    implementation("io.quarkus:quarkus-smallrye-health")   // /q/health/ready readiness probe (install.ps1 polls it)

    implementation(project(":inventory"))          // inventory migrations, event sink, REST, admin item
    implementation(project(":admin"))              // /admin console + REST fan-out to remote characters
    implementation(project(":characters-client"))  // REMOTE PlayerCharacters producer (edge/QUIC client)
    implementation(project(":platform"))           // RoleConfig
    implementation(project(":arch-rules"))          // Layer 3 (opt-in demo): build-time architecture-validation extension
    // NOTE: no :characters or :accounts impl — proven by :inventory-service:dependencies.
}
