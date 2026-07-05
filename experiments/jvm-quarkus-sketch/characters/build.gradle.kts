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
    implementation("io.quarkus:quarkus-messaging-kafka") // SmallRye Reactive Messaging (internal channel until Step 7 connectors)
    implementation("io.quarkus:quarkus-scheduler")       // @Scheduled outbox relay
    implementation("io.quarkus:quarkus-jackson")         // ObjectMapper bean for payload (de)serialization
    implementation("io.quarkus:quarkus-grpc")            // @GrpcService server + @GrpcClient in the produced adapter
    implementation("io.quarkus:quarkus-rest")            // GET /admin-data/characters
    implementation("io.quarkus:quarkus-rest-jackson")    // JSON serialization of AdminItemDto

    api(project(":characters-api"))      // @Produces PlayerCharacters (the produced capability bean)
    api(project(":characters-events"))   // CharacterCreated/Deleted payloads (emitted via outbox relay)
    implementation(project(":characters-grpc"))   // generated PlayerCharacters gRPC/Mutiny stubs (server + client)
    api(project(":admin-api"))           // @Produces Item (public return type)
    implementation(project(":platform"))
}

allOpen {
    annotation("jakarta.enterprise.context.ApplicationScoped")
    annotation("jakarta.persistence.Entity")
    annotation("io.quarkus.grpc.GrpcService")   // @GrpcService bean must be proxyable/open
}
