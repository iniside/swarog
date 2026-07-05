// characters-client — the REMOTE PlayerCharacters producer (edge-RPC over QUIC). A plain library
// (NO io.quarkus plugin, only the app-shells have it); beans.xml makes its producer discoverable
// across the jar boundary. It depends ONLY on the `characters-api` contract + `edge` transport, NEVER
// on the `characters` impl — so a service that includes this module gets a PlayerCharacters that dials
// the remote QUIC server, with no chance of pulling in the characters implementation. Bean PRESENCE
// (this module on the classpath), not RoleConfig, is what makes a process a characters CLIENT.
plugins {
    kotlin("jvm")
    kotlin("plugin.allopen")   // @ApplicationScoped producer bean must be open (CDI client proxy)
}

val quarkusPlatformGroupId: String by project
val quarkusPlatformArtifactId: String by project
val quarkusPlatformVersion: String by project

dependencies {
    implementation(enforcedPlatform("$quarkusPlatformGroupId:$quarkusPlatformArtifactId:$quarkusPlatformVersion"))
    implementation("io.quarkus:quarkus-kotlin")
    implementation("io.quarkus:quarkus-arc")   // @ApplicationScoped + @Produces + @ConfigProperty

    api(project(":characters-api"))       // the PlayerCharacters interface + OwnerOf DTOs (produced type)
    implementation(project(":edge"))      // edge RPC core + MsQuicClientTransport (ownerOf over QUIC)
    implementation(project(":platform"))  // shared infra slice (parity with the other modules)
}

allOpen {
    annotation("jakarta.enterprise.context.ApplicationScoped")
}
