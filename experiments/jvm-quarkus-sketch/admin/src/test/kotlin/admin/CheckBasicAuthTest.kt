package admin

import java.util.Base64
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * Unit test for the pure Basic-auth gate ([checkBasicAuth]) extracted from [AdminResource.unauthorized]
 * (seam #3) — exercisable without setting a JVM env var. The headline case is §Bugs #1: a MALFORMED
 * Base64 `Authorization` header must be rejected (→ 401 at the caller), NOT throw
 * [IllegalArgumentException] out of [Base64.getDecoder] (→ a 500). The other cases pin the surrounding
 * decode/compare logic so the fix can't regress it.
 */
class CheckBasicAuthTest {

    private fun basic(user: String, pass: String): String =
        "Basic " + Base64.getEncoder().encodeToString("$user:$pass".toByteArray())

    @Test
    fun `malformed Base64 header is rejected, not thrown (Bugs 1)`() {
        // '@@@' is not valid Base64 — the pre-fix code threw IllegalArgumentException here (=> 500).
        assertFalse(checkBasicAuth("Basic @@@not-base64@@@", expectedUser = "admin", expectedPass = "secret"))
    }

    @Test
    fun `valid credentials are accepted`() {
        assertTrue(checkBasicAuth(basic("admin", "secret"), expectedUser = "admin", expectedPass = "secret"))
    }

    @Test
    fun `wrong credentials are rejected`() {
        assertFalse(checkBasicAuth(basic("admin", "wrong"), expectedUser = "admin", expectedPass = "secret"))
    }

    @Test
    fun `an unset gate (null expectedUser) is open`() {
        assertTrue(checkBasicAuth(authHeader = null, expectedUser = null, expectedPass = null))
    }

    @Test
    fun `a missing Authorization header is rejected when the gate is set`() {
        assertFalse(checkBasicAuth(authHeader = null, expectedUser = "admin", expectedPass = "secret"))
    }

    @Test
    fun `a non-Basic scheme is rejected`() {
        assertFalse(checkBasicAuth("Bearer sometoken", expectedUser = "admin", expectedPass = "secret"))
    }
}
