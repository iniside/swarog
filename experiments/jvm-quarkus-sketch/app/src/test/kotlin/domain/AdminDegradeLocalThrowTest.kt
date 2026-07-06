package domain

import characters.CharactersAdminData
import io.quarkus.test.InjectMock
import io.quarkus.test.junit.QuarkusTest
import io.restassured.RestAssured.given
import org.hamcrest.Matchers.containsString
import org.junit.jupiter.api.Test
import org.mockito.Mockito

/**
 * P0-ADMIN-DEGRADE (local branch): when a LOCALLY-hosted module's [admin.adminapi.AdminDataProvider.data]
 * throws (arbitrary module code), admin must isolate it to an error card, not 500 the whole page. Uses
 * the Step-3a-proven substitution (`@InjectMock` on one bean in `@All List<AdminDataProvider>` — it DOES
 * appear in the list): [CharactersAdminData] is stubbed to throw from `data()` while keeping its real
 * id/section/label (so admin still routes to it via its `id`), and the console still renders at 200.
 */
@QuarkusTest
class AdminDegradeLocalThrowTest {

    @InjectMock
    lateinit var charactersAdmin: CharactersAdminData

    @Test
    fun `a throwing local provider renders an error card at 200, not a 500`() {
        Mockito.`when`(charactersAdmin.id).thenReturn("characters")
        Mockito.`when`(charactersAdmin.section).thenReturn("Game Content")
        Mockito.`when`(charactersAdmin.label).thenReturn("Characters")
        Mockito.`when`(charactersAdmin.data()).thenThrow(IllegalStateException("provider boom"))

        given()
            .`when`().get("/admin/characters")
            .then()
            .statusCode(200)
            .body(containsString("unavailable"))
    }
}
