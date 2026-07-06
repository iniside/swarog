package domain

import io.quarkus.test.junit.QuarkusTest
import io.quarkus.test.junit.TestProfile
import io.quarkus.test.junit.QuarkusTestProfile
import io.restassured.RestAssured.given
import org.hamcrest.Matchers.containsString
import org.junit.jupiter.api.Test

/**
 * P0-ADMIN-DEGRADE (remote branch): a module that is NOT hosted in this process (so admin fetches its
 * dashboard over REST from `admin.<id>.url`) must degrade to an ERROR CARD when that peer is down —
 * `/admin` renders HTTP 200, never a 500 that would blank the whole console. Driven purely by config
 * (the one profile enumerated below): `roles` excludes `characters` so `roleConfig.isActive("characters")`
 * is false and [admin.AdminResource]'s fan-out takes the REMOTE branch, and `admin.characters.url`
 * points at a dead port so the fetch fails.
 */
@QuarkusTest
@TestProfile(AdminDegradeRemoteTest.RemoteDownProfile::class)
class AdminDegradeRemoteTest {

    /**
     * The single @TestProfile this suite adds (justified: the remote branch is only reachable when a
     * module in `admin.modules` is INACTIVE, which needs a non-`all` `roles` set — not overridable
     * per-request). Excludes `characters` from `roles`, lists only it in `admin.modules`, and points
     * its admin URL at a closed port.
     */
    class RemoteDownProfile : QuarkusTestProfile {
        override fun getConfigOverrides(): Map<String, String> = mapOf(
            "roles" to "accounts,inventory,admin",
            "admin.modules" to "characters",
            "admin.characters.url" to "http://localhost:1/",
        )
    }

    @Test
    fun `a down remote module renders an error card at 200, not a 500`() {
        given()
            .`when`().get("/admin/characters")
            .then()
            .statusCode(200)
            .body(containsString("unavailable"))
    }
}
