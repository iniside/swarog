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
    implementation("io.quarkus:quarkus-messaging-kafka") // SmallRye Reactive Messaging (@Incoming consumers)
    implementation("io.quarkus:quarkus-scheduler")       // shares the messaging/scheduler runtime with the publishers
    implementation("io.quarkus:quarkus-jackson")         // ObjectMapper bean on the compile classpath
    implementation("io.quarkus:quarkus-grpc")            // gRPC client runtime (ownerOf goes remote in the split)
    implementation("io.quarkus:quarkus-rest")            // InventoryResource + GET /admin-data/inventory
    implementation("io.quarkus:quarkus-rest-jackson")    // JSON serialization of AdminItemDto

    api(project(":characters-api"))      // injects PlayerCharacters (public ctor)
    api(project(":characters-events"))   // @ObservesAsync CharacterCreated/Deleted (public params)
    implementation(project(":characters-grpc"))   // gRPC stubs shared with the characters server
    api(project(":admin-api"))           // @Produces Item
    implementation(project(":platform"))
}

allOpen {
    annotation("jakarta.enterprise.context.ApplicationScoped")
    annotation("jakarta.persistence.Entity")
}
