// Cross-cutting infra shared by impl modules — no feature knowledge. Now hosts RoleConfig, a
// CDI bean, so it carries the Quarkus BOM + quarkus-arc (@ApplicationScoped, @ConfigProperty)
// and allopen; it still has NO io.quarkus plugin (only `app` does). beans.xml makes its beans
// discoverable across the jar boundary. Later steps add the outbox row-model/mark-sent helper.
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
    implementation("io.quarkus:quarkus-arc")   // @ApplicationScoped + MicroProfile @ConfigProperty
}

allOpen {
    annotation("jakarta.enterprise.context.ApplicationScoped")
}
