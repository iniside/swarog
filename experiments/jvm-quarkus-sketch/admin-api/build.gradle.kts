// The admin contribution contract (Item/SectionData/Kpi/Table/Cell) — plain DTOs, no Quarkus.
plugins {
    kotlin("jvm")
}

// Contract module — explicitApi() forces explicit visibility + return types on the shared surface
// so the admin seam can't drift implicitly (verification Layer 0).
kotlin {
    explicitApi()
}
