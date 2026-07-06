// inventory impl — owns schema "inventory", the Holding @Entity, SYNC-asks PlayerCharacters,
// REACTS to character events, contributes an admin Item. No io.quarkus plugin (only `app`).
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
    implementation("io.quarkus:quarkus-narayana-jta")   // @Transactional write paths
    implementation("io.quarkus:quarkus-jackson")         // ObjectMapper bean on the compile classpath
    implementation("io.quarkus:quarkus-smallrye-stork")  // Stork name-resolver for the admin REST fan-out (characters-service)
    implementation("io.smallrye.stork:stork-service-discovery-static-list") // static address-list discovery (BOM-managed version)
    implementation("io.quarkus:quarkus-rest")            // InventoryResource + GET /admin-data/inventory
    implementation("io.quarkus:quarkus-rest-jackson")    // JSON serialization of AdminItemDto

    api(project(":characters-api"))      // injects PlayerCharacters (public ctor)
    api(project(":characters-events"))   // @ObservesAsync CharacterCreated/Deleted (public params)
    implementation(project(":edge"))     // edge RPC core + MsQuicClientTransport (ownerOf over QUIC)
    api(project(":admin-api"))           // @Produces Item
    implementation(project(":platform"))

    testImplementation("org.junit.jupiter:junit-jupiter:5.11.0")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

allOpen {
    annotation("jakarta.enterprise.context.ApplicationScoped")
    annotation("jakarta.persistence.Entity")
}
