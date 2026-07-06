// Cross-cutting infra shared by impl modules — no feature knowledge. Now hosts RoleConfig, a
// CDI bean, so it carries the Quarkus BOM + quarkus-arc (@ApplicationScoped, @ConfigProperty)
// and allopen; it still has NO io.quarkus plugin (only `app` does). beans.xml makes its beans
// discoverable across the jar boundary. Later steps add the outbox row-model/mark-sent helper.
plugins {
    kotlin("jvm")
    kotlin("plugin.allopen")
    id("info.solidsoft.pitest")
}

val quarkusPlatformGroupId: String by project
val quarkusPlatformArtifactId: String by project
val quarkusPlatformVersion: String by project

dependencies {
    implementation(enforcedPlatform("$quarkusPlatformGroupId:$quarkusPlatformArtifactId:$quarkusPlatformVersion"))
    implementation("io.quarkus:quarkus-kotlin")
    implementation("io.quarkus:quarkus-arc")   // @ApplicationScoped + MicroProfile @ConfigProperty

    testImplementation("org.junit.jupiter:junit-jupiter:5.11.0")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

allOpen {
    annotation("jakarta.enterprise.context.ApplicationScoped")
}

// PITest — mutation testing, best-effort/capped (Step 6 Part B). The plugin is applied but NOT wired
// into `check`/`build` (no task depends on `pitest`) — deliberately: it does not run on this JDK.
// Scoped to `RoleConfig` (pure logic, no CDI container needed to exercise `isActive`/`isMonolith`) +
// its existing `RoleConfigTest`, ready to re-enable the day PIT ships an ASM upgrade.
//
// STATUS: DEFERRED on JDK 26 (verified 2026-07-06). `./gradlew :platform:pitest` fails hard in PIT's
// own analysis phase:
//   java.lang.IllegalArgumentException: Unsupported class file major version 70
//       at org.objectweb.asm.ClassReader.<init>(ClassReader.java:200)
//       at org.pitest.bytecode.analysis.ClassTree.fromBytes(ClassTree.java:39)
// This is NOT the detekt-class issue (a vendored parser reading the `java.version` *system property*,
// fixed there by spoofing it for the task). Here ASM's `ClassReader` does a hard numeric check against
// the major-version byte actually stored in each already-compiled `.class` file (70 = JDK 26); no
// system property or alternate JVM launcher for the pitest task itself changes what version byte is
// IN the class file it reads, so the detekt-style spoof (tried first) has no effect and a JDK-21
// launcher for the pitest task doesn't either. The only real fix would be compiling a JDK-21-target
// copy of `platform`'s classes solely for PIT to scan — out of scope for a 30-minute capped, PIT 1.19.0
// (latest as of this writing) predates ASM 9.9/JDK 26 class-file support (hcoles/pitest tracks this
// generally; no released fix). Kover (Part A, wired into `check`) is the real coverage gate; this stays
// a manual, unwired opt-in until PIT catches up.
pitest {
    pitestVersion.set(project.property("pitestVersion") as String)
    junit5PluginVersion.set(project.property("pitestJunit5PluginVersion") as String)
    targetClasses.set(setOf("platform.RoleConfig"))
    targetTests.set(setOf("platform.RoleConfigTest"))
    mutationThreshold.set(40)
}
