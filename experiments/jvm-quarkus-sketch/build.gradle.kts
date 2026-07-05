import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.dsl.KotlinJvmProjectExtension

// Parent build: the module boundaries are now PHYSICAL (one Gradle module per architectural
// module). Only `app` applies `io.quarkus`; library modules compile against Quarkus via the BOM.
// Plugins are declared here `apply false` so subprojects resolve them by id without a version
// (versions come from settings.gradle.kts pluginManagement).
plugins {
    kotlin("jvm") apply false
    kotlin("plugin.allopen") apply false
    kotlin("plugin.jpa") apply false
    id("io.quarkus") apply false
}

// Common config for every Kotlin module — repositories, jvmTarget=JVM_26, java toolchain 26,
// JUnit platform for tests. Applied reactively when the kotlin-jvm plugin lands on a subproject.
subprojects {
    repositories { mavenCentral() }

    plugins.withId("org.jetbrains.kotlin.jvm") {
        extensions.configure<KotlinJvmProjectExtension> {
            compilerOptions {
                jvmTarget = JvmTarget.JVM_26
                // Strict-compile canaries (verification Layer 0): every warning is a build
                // failure, progressive mode opts into the newest deprecation/resolution rules,
                // and -Xjsr305=strict treats JSR-305 nullability as hard (cheap, low-yield here —
                // Jakarta/Panache carry no JSR-305 annotations, so it is not a null-interop fix).
                allWarningsAsErrors.set(true)
                progressiveMode.set(true)
                freeCompilerArgs.add("-Xjsr305=strict")
            }
        }
        extensions.configure<JavaPluginExtension> {
            toolchain { languageVersion = JavaLanguageVersion.of(26) }
        }
        tasks.withType<Test>().configureEach { useJUnitPlatform() }
    }
}
