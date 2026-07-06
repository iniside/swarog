package admin

import admin.adminapi.AdminItemDto
import admin.adminapi.SectionData
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * White-box GOLDEN-MASTER of the whole-list [slugify] (seam #2) — the pure slug+dedupe that
 * `resolve()` runs over the ALREADY-SORTED item list to give each dashboard a unique `/admin/{slug}`.
 *
 * These expectations pin the slug rule to Go's `modules/admin/admin.go#slugify`, mirrored 1:1:
 *   lowercase, keep [a-z0-9], map space / '-' / '_' → '-', DROP every other rune, trim leading/
 *   trailing '-'; the whole-list caller then dedupes collisions (`-2`/`-3`/…) and falls an empty
 *   base back to "item". Order is PRESERVED from the (pre-sorted) input — [slugify] never re-sorts.
 *
 * History: the previous commit golden-mastered the OLD, buggy Kotlin rule (literal-space→'-' only),
 * which let a '/' in a label survive into the slug and break `/admin/{slug}` routing (§Bugs #5).
 * This commit swaps the rule to the Go allowlist and updates these expectations, so the change is
 * provably intentional rather than an accidental behavior drift.
 */
class SlugifyTest {

    /** An [AdminItemDto] whose only interesting field is its label (id/section/data are inert). */
    private fun item(label: String, section: String = "S"): AdminItemDto =
        AdminItemDto(id = label, section = section, label = label, data = SectionData())

    /** Slug the given labels IN ORDER (they are treated as already sorted by the caller). */
    private fun slugsOf(vararg labels: String): List<String> =
        slugify(labels.map { item(it) }).map { it.slug }

    // ---- single-label rule (Go allowlist) -----------------------------------

    @Test
    fun `multi-word phrase lowercases and maps space to dash`() {
        assertEquals(listOf("game-content"), slugsOf("Game Content"))
    }

    @Test
    fun `single word is only lowercased`() {
        assertEquals(listOf("players"), slugsOf("Players"))
    }

    @Test
    fun `uppercase is folded to lowercase`() {
        assertEquals(listOf("hello"), slugsOf("HELLO"))
    }

    @Test
    fun `digits survive`() {
        assertEquals(listOf("zone42"), slugsOf("Zone42"))
    }

    @Test
    fun `existing dashes are kept`() {
        assertEquals(listOf("hello-world"), slugsOf("hello-world"))
    }

    @Test
    fun `underscores map to dash and are trimmed at the ends`() {
        // "_leading_" → '-' 'leading' '-' → trimmed → "leading" (matches Go's slugify test).
        assertEquals(listOf("leading"), slugsOf("_leading_"))
    }

    @Test
    fun `apostrophes and bangs are dropped, not slugged`() {
        // "Foo's Bar!" → 'f','o','o', apostrophe dropped, 's' kept → "foos"; space→'-'; 'b','a','r',
        // bang dropped → "foos-bar". Punctuation vanishes but the surrounding letters remain.
        assertEquals(listOf("foos-bar"), slugsOf("Foo's Bar!"))
    }

    @Test
    fun `dropped punctuation leaves only the space-derived dashes`() {
        // "A/B & C" → 'a' 'b' (slash dropped) '-' (space) '-' (space, ampersand dropped) 'c'
        // → "ab--c" — the two spaces each become a dash; '/' and '&' vanish (matches Go's test).
        assertEquals(listOf("ab--c"), slugsOf("A/B & C"))
    }

    // ---- empty-base fallback (drop-to-empty now reaches the fallback) --------

    @Test
    fun `a genuinely empty label falls back to item`() {
        assertEquals(listOf("item"), slugsOf(""))
    }

    @Test
    fun `an all-symbol label drops to empty then falls back to item`() {
        // Every rune is dropped by the allowlist → "" → "item" (was passed through verbatim before).
        assertEquals(listOf("item"), slugsOf("!@#\$%^&*()"))
    }

    @Test
    fun `a double space trims to empty then falls back to item`() {
        // "  " → "--" → trimmed to "" → "item" (was "--" under the old space-only rule).
        assertEquals(listOf("item"), slugsOf("  "))
    }

    // ---- routing safety (§Bugs #5, the reason for the rule change) -----------

    @Test
    fun `a slash in a label never survives into the slug so admin slug routing works`() {
        // The regression the rule change closes: a '/' in the slug is a second path segment and
        // GET /admin/{slug} can never match it → the module page 404s. The allowlist drops '/'.
        val slug = slugsOf("Foo/Bar").single()
        assertFalse(slug.contains('/'), "slug of a '/'-label must not contain '/': was '$slug'")
        assertEquals("foobar", slug)
    }

    // ---- whole-list dedupe + order (unchanged by the rule swap) --------------

    @Test
    fun `a duplicate label gets a -2 suffix, keeping the first as the base`() {
        assertEquals(listOf("players", "players-2"), slugsOf("Players", "Players"))
    }

    @Test
    fun `three colliding labels number -2 then -3`() {
        assertEquals(listOf("players", "players-2", "players-3"), slugsOf("Players", "Players", "Players"))
    }

    @Test
    fun `distinct empty-base labels collide through the item fallback`() {
        // Both "" and "!!!" now slug to the "item" base, so the second is disambiguated to "item-2".
        assertEquals(listOf("item", "item-2"), slugsOf("", "!!!"))
    }

    @Test
    fun `input order is preserved (slugify does not re-sort)`() {
        // slugify consumes the ALREADY-sorted list; it must not reorder — the sort is resolve()'s job.
        assertEquals(listOf("zebra", "alpha"), slugsOf("Zebra", "Alpha"))
    }

    @Test
    fun `all produced slugs are unique`() {
        val slugs = slugsOf("Players", "Players", "", "!!!", "Game Content")
        assertEquals(slugs.size, slugs.toSet().size, "expected all slugs distinct, got $slugs")
    }

    @Test
    fun `each Resolved carries its originating dto`() {
        val dto = item("Leaderboard")
        val resolved = slugify(listOf(dto)).single()
        assertTrue(resolved.dto === dto, "Resolved must reference the same dto instance it slugged")
        assertEquals("leaderboard", resolved.slug)
    }
}
