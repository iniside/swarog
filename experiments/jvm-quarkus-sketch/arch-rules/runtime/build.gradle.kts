// Verification Layer 3 (OPT-IN demo) — the RUNTIME half of the arch-rules Quarkus extension.
//
// It is deliberately EMPTY: no runtime code, no beans. Its only job is to carry the generated
// `META-INF/quarkus-extension.properties` descriptor (written by the io.quarkus.extension plugin)
// whose `deployment=` line points at the `arch-rules-deployment` module. That descriptor is how
// Quarkus discovers the deployment half: an app-shell puts THIS jar on its runtime classpath
// (`implementation(project(":arch-rules"))`) and augmentation then loads arch-rules-deployment onto
// the build classpath and runs its @BuildStep validators.
//
// The plugin resolves the deployment module by LOCAL PROJECT NAME (ToolingUtils.findLocalProject),
// so `deploymentModule` MUST equal the deployment project's name ("arch-rules-deployment") — a
// mismatch is a SILENT no-op (no validation, green build). group/version are set on both halves so
// the generated deployment-artifact GAV is well-formed in this multi-module build.
plugins {
    id("io.quarkus.extension")
}

val quarkusPlatformGroupId: String by project
val quarkusPlatformArtifactId: String by project
val quarkusPlatformVersion: String by project

group = "gamebackend.archrules"
version = "1.0.0"

quarkusExtension {
    deploymentModule.set("arch-rules-deployment")
}

dependencies {
    implementation(enforcedPlatform("$quarkusPlatformGroupId:$quarkusPlatformArtifactId:$quarkusPlatformVersion"))
    implementation("io.quarkus:quarkus-arc")
}

java {
    toolchain { languageVersion = JavaLanguageVersion.of(26) }
}
