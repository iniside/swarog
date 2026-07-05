// Synchronous capability contract of the characters domain (PlayerCharacters) — plain interface.
plugins {
    kotlin("jvm")
}

// This is a CONTRACT module — its whole reason to exist is its public surface. explicitApi()
// makes every public declaration state its visibility + return type, so the shared API can't
// drift implicitly (verification Layer 0). Only the four contract modules carry this.
kotlin {
    explicitApi()
}
