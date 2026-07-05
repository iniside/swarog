// Published event contracts of the characters domain — plain data classes, no io.quarkus plugin.
// Depends on quarkus-core (via the BOM) only for @RegisterForReflection so the payloads survive
// native-image JSON serde; no CDI, no allopen (these are data classes, not beans).
plugins {
    kotlin("jvm")
}

val quarkusPlatformGroupId: String by project
val quarkusPlatformArtifactId: String by project
val quarkusPlatformVersion: String by project

dependencies {
    api(enforcedPlatform("$quarkusPlatformGroupId:$quarkusPlatformArtifactId:$quarkusPlatformVersion"))
    api("io.quarkus:quarkus-core")   // @RegisterForReflection (part of the public payload API)
}
