// Verification Layer 3 (OPT-IN demo) — the DEPLOYMENT half of the arch-rules Quarkus extension.
//
// JAVA SOURCES ONLY (src/main/java). This is NON-NEGOTIABLE: quarkus-extension-processor does NOT
// index Kotlin @BuildStep classes (quarkusio/quarkus#35110) — the generated
// `META-INF/quarkus-build-steps.list` comes out EMPTY and the validators SILENTLY never run. A green
// build would then prove nothing. (The non-empty build-steps.list is checked as a canary; the
// standing negative-fixture QuarkusUnitTest in src/test/java is the real liveness proof.)
//
// The build steps re-implement Layer-1's architecture checks against ArC's AUGMENTED model
// (ValidationPhaseBuildItem.beans()) + the combined Jandex index, producing ValidationErrorBuildItem
// so a violation fails `quarkusBuild`. They reference the domain types by Jandex DotName string only,
// so this module has NO compile dependency on any feature module — it stays a decoupled validator.
plugins {
    java
}

val quarkusPlatformGroupId: String by project
val quarkusPlatformArtifactId: String by project
val quarkusPlatformVersion: String by project

group = "gamebackend.archrules"
version = "1.0.0"

dependencies {
    implementation(enforcedPlatform("$quarkusPlatformGroupId:$quarkusPlatformArtifactId:$quarkusPlatformVersion"))

    // The deployment APIs the build steps consume: core (CombinedIndexBuildItem) + arc
    // (ValidationPhaseBuildItem / ValidationErrorBuildItem).
    implementation("io.quarkus:quarkus-core-deployment")
    implementation("io.quarkus:quarkus-arc-deployment")

    // Generates META-INF/quarkus-build-steps.list from the @BuildStep methods (Java only — see #35110).
    annotationProcessor("io.quarkus:quarkus-extension-processor")

    // The runtime half — an extension's deployment module conventionally depends on its runtime.
    implementation(project(":arch-rules"))

    // Negative-fixture liveness test: QuarkusUnitTest runs a real in-JVM augmentation of a synthetic
    // app that DELIBERATELY violates a rule and asserts augmentation FAILS. quarkus-junit5-internal
    // provides QuarkusUnitTest; quarkus-arc + admin-api give the synthetic app a bean container and the
    // real AdminDataProvider contract the bad bean implements (so getAllKnownImplementors finds it).
    testImplementation("io.quarkus:quarkus-junit5-internal")
    testImplementation("io.quarkus:quarkus-arc")
    testImplementation(project(":admin-api"))
    testImplementation("org.junit.jupiter:junit-jupiter:5.11.0")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

java {
    toolchain { languageVersion = JavaLanguageVersion.of(26) }
}

tasks.withType<Test>().configureEach { useJUnitPlatform() }
