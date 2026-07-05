// The runnable: the ONLY io.quarkus module. Aggregates the impl modules (their Quarkus
// extensions ride the runtime classpath transitively, so augmentation discovers every
// extension/entity/bean), holds application.properties + the demo Seed, and runs quarkusBuild.
plugins {
    kotlin("jvm")
    kotlin("plugin.allopen")   // Seed is @ApplicationScoped
    id("io.quarkus")
}

val quarkusPlatformGroupId: String by project
val quarkusPlatformArtifactId: String by project
val quarkusPlatformVersion: String by project

dependencies {
    implementation(enforcedPlatform("$quarkusPlatformGroupId:$quarkusPlatformArtifactId:$quarkusPlatformVersion"))
    implementation("io.quarkus:quarkus-kotlin")
    implementation("io.quarkus:quarkus-arc")   // Seed: StartupEvent, @Observes, @Priority

    implementation(project(":accounts"))
    implementation(project(":characters"))
    implementation(project(":inventory"))
    implementation(project(":admin"))
    implementation(project(":platform"))   // Seed injects RoleConfig to gate the demo seed

    // Same architecture rules as the framework-free sketch — constraints outlive the framework.
    testImplementation("com.tngtech.archunit:archunit:1.4.2")
    testImplementation("org.junit.jupiter:junit-jupiter:5.11.0")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

allOpen {
    annotation("jakarta.enterprise.context.ApplicationScoped")
}
