// Shared gRPC stub module: owns characters.proto and generates the protobuf messages, the standard
// gRPC-Java stubs AND the Quarkus Mutiny stubs (blocking base + Mutiny stub) that @GrpcService /
// @GrpcClient expect. Quarkus's own codegen is tied to the io.quarkus plugin (only on `app`), so a
// dedicated module here runs codegen via the com.google.protobuf plugin wired to the
// quarkus-grpc-protoc-plugin (Mutiny generator). Both `characters` (server) and `inventory` (client)
// depend on this — no io.quarkus plugin, no CDI beans (so no beans.xml), just generated types.
import com.google.protobuf.gradle.id

plugins {
    kotlin("jvm")
    id("com.google.protobuf") version "0.10.0"   // first release with full Gradle 9 support
}

val quarkusPlatformGroupId: String by project
val quarkusPlatformArtifactId: String by project
val quarkusPlatformVersion: String by project

dependencies {
    // BOM manages grpc/protobuf/mutiny/quarkus-grpc versions; api() so consumers see the generated
    // code's transitive runtime types (Uni, grpc stubs, protobuf messages, @Generated).
    api(enforcedPlatform("$quarkusPlatformGroupId:$quarkusPlatformArtifactId:$quarkusPlatformVersion"))
    api("io.grpc:grpc-protobuf")
    api("io.grpc:grpc-stub")
    api("com.google.protobuf:protobuf-java")
    api("io.smallrye.reactive:mutiny")            // generated Mutiny stubs return Uni<T>
    api("io.quarkus:quarkus-grpc-api")            // MutinyService / MutinyStub / MutinyGrpc / @GrpcService on generated code
    api("io.quarkus:quarkus-grpc-stubs")          // io.quarkus.grpc.stubs.ClientCalls / ServerCalls referenced by generated stubs
    api("jakarta.annotation:jakarta.annotation-api")   // @jakarta.annotation.Generated on stubs
}

// protoc tooling artifacts are resolved by the protobuf plugin OUTSIDE dependency management, so they
// carry explicit versions matched to the Quarkus 3.37.1 BOM (grpc 1.81.0, protobuf/protoc 4.35.0).
protobuf {
    protoc {
        artifact = "com.google.protobuf:protoc:4.35.0"
    }
    plugins {
        id("grpc") {
            artifact = "io.grpc:protoc-gen-grpc-java:1.81.0"
        }
        id("quarkus") {
            // The shaded (fat) jar is the runnable generator; protobuf-gradle-plugin wraps an
            // @jar plugin in a `java -jar` launcher. Main-Class: io.quarkus.grpc.protoc.plugin.MutinyGrpcGenerator.
            artifact = "io.quarkus:quarkus-grpc-protoc-plugin:3.37.1:shaded@jar"
        }
    }
    generateProtoTasks {
        all().forEach { task ->
            task.plugins {
                id("grpc")     // GreeterGrpc-style blocking/async stubs
                id("quarkus")  // MutinyGreeterGrpc: Mutiny base (@GrpcService) + Mutiny stub (@GrpcClient)
            }
        }
    }
}
