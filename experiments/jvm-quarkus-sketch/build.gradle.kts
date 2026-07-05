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
            compilerOptions { jvmTarget = JvmTarget.JVM_26 }
        }
        extensions.configure<JavaPluginExtension> {
            toolchain { languageVersion = JavaLanguageVersion.of(26) }
        }
        tasks.withType<Test>().configureEach { useJUnitPlatform() }
    }
}
