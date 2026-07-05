import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    // The whole "framework-free" point: this is the entire plugin list.
    kotlin("jvm") version "2.4.0"
    application
}

repositories { mavenCentral() }

dependencies {
    // No Spring, no Micronaut, no Netty, no DI container. (JDBC + HTTP server are in the JDK.)
    implementation("org.postgresql:postgresql:42.7.5")
    // HTML templating for the admin panel — single jar, ZERO transitive dependencies.
    implementation("org.freemarker:freemarker:2.3.34")

    // Architecture rules as tests — enforce the module boundaries inside this SINGLE jar.
    testImplementation("com.tngtech.archunit:archunit:1.4.2")
    testImplementation("org.junit.jupiter:junit-jupiter:5.11.0")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

tasks.test { useJUnitPlatform() }

kotlin {
    compilerOptions {
        // Full JDK 26: emit JVM 26 bytecode (Kotlin 2.4.0 added the JVM_26 target).
        jvmTarget = JvmTarget.JVM_26
    }
}

java {
    // Build AND run on a JDK 26 toolchain. Gradle 9.6.1 supports running on JDK 26.
    toolchain { languageVersion = JavaLanguageVersion.of(26) }
}

application {
    mainClass = "app.MainKt"
}
