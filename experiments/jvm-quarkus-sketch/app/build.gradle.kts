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
    implementation("io.quarkus:quarkus-jackson")   // JSON serde for event payloads (additive-evolution config)
    // Jackson can serialize a Kotlin data class via getters, but DESERIALIZING one (constructor-based,
    // no no-arg ctor) needs the Kotlin module. quarkus-jackson does NOT pull it in; without it the inventory
    // event-sink REST endpoint's JSON->CharacterCreated body binding fails at runtime (InvalidDefinitionException:
    // no Creators). Present on the classpath, Quarkus auto-registers KotlinModule on the single ObjectMapper bean.
    implementation("com.fasterxml.jackson.module:jackson-module-kotlin")
    implementation("io.quarkus:quarkus-smallrye-health")   // /q/health/ready readiness probe (install.ps1 polls it)

    implementation(project(":accounts"))
    implementation(project(":characters"))
    implementation(project(":inventory"))
    implementation(project(":admin"))
    implementation(project(":platform"))   // Seed injects RoleConfig to gate the demo seed
    implementation(project(":arch-rules"))  // Layer 3 (opt-in demo): build-time architecture-validation extension

    // Same architecture rules as the framework-free sketch — constraints outlive the framework.
    testImplementation("com.tngtech.archunit:archunit:1.4.2")
    testImplementation("org.junit.jupiter:junit-jupiter:5.11.0")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
    // Layer 2 (Konsist) rules: `KonsistArchitectureTest` inspects `@Path` (JAX-RS) directly, so
    // the JAX-RS API needs to be on app's OWN test classpath — `characters`/`inventory`/`admin`
    // declare quarkus-rest as `implementation`, which Gradle does NOT expose transitively to a
    // consumer's (app's) classpath. De-risked first: Konsist embeds a K2 front-end
    // (kotlin-compiler-embeddable); its JDK-26-toolchain compatibility was unproven before a
    // trivial `Konsist.scopeFromProject().classes().assertTrue { true }` ran green.
    testImplementation("com.lemonappdev:konsist:0.17.3")
    testImplementation("io.quarkus:quarkus-rest")
}

allOpen {
    annotation("jakarta.enterprise.context.ApplicationScoped")
}
