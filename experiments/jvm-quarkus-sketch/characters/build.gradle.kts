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
    implementation("io.quarkus:quarkus-narayana-jta")

    api(project(":characters-api"))      // implements PlayerCharacters (supertype)
    api(project(":characters-events"))   // Event<CharacterCreated/Deleted> in public API
    api(project(":admin-api"))           // @Produces Item (public return type)
    implementation(project(":platform"))
}

allOpen {
    annotation("jakarta.enterprise.context.ApplicationScoped")
    annotation("jakarta.persistence.Entity")
}
