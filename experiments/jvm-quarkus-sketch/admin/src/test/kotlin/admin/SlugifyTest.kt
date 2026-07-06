package admin

import admin.adminapi.AdminItemDto
import admin.adminapi.SectionData
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * White-box GOLDEN-MASTER of the whole-list [slugify] (seam #2) — the pure slug+dedupe that
 * `resolve()` runs over the ALREADY-SORTED item list to give each dashboard a unique `/admin/{slug}`.
 *
 * This commit pins the slug rule EXACTLY AS IT IS TODAY: `label.lowercase().replace(" ", "-")` with
 * an empty→"item" fallback and whole-list collision numbering (`-2`/`-3`/…). That rule is BUGGY — it
 * maps ONLY the literal space and passes every other rune through untouched — so this golden-master
 * deliberately DOCUMENTS the bugs (a '/' in a label survives into the slug and breaks
 * `/admin/{slug}` routing; arbitrary punctuation and double-spaces leak through). A follow-up commit
 * swaps the rule to Go's allowlist and updates these expectations, so the change is provably
 * intentional rather than an accidental behavior drift.
 */
class SlugifyTest {

    /** An [AdminItemDto] whose only interesting field is its label (id/section/data are inert). */
    private fun item(label: String, section: String = "S"): AdminItemDto =
        AdminItemDto(id = label, section = section, label = label, data = SectionData())

    /** Slug the given labels IN ORDER (they are treated as already sorted by the caller). */
    private fun slugsOf(vararg labels: String): List<String> =
        slugify(labels.map { item(it) }).map { it.slug }

    // ---- single-label rule: lowercase + literal-space→dash ONLY -------------

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

    // ---- DOCUMENTED BUGS: everything but the space passes through -----------

    @Test
    fun `BUG a slash in the label survives into the slug (breaks admin routing)`() {
        // "Foo/Bar" → lowercased "foo/bar"; only the space rule fires, so the '/' is untouched.
        // A '/' makes "foo/bar" a two-segment path that GET /admin/{slug} can never match → 404.
        val slug = slugsOf("Foo/Bar").single()
        assertEquals("foo/bar", slug)
        assertTrue(slug.contains('/'), "documents the routing bug: the slug currently keeps its '/'")
    }

    @Test
    fun `BUG underscores are NOT mapped to dash`() {
        // Under the Go allowlist this would become "leading"; today underscores pass through verbatim.
        assertEquals(listOf("_leading_"), slugsOf("_leading_"))
    }

    @Test
    fun `BUG arbitrary punctuation survives verbatim`() {
        // "Foo's Bar!" keeps the apostrophe and the bang; only the space becomes a dash.
        assertEquals(listOf("foo's-bar!"), slugsOf("Foo's Bar!"))
    }

    @Test
    fun `BUG only spaces are dashed, so slashes and ampersands stay`() {
        // "A/B & C" → "a/b-&-c": the two spaces become dashes, '/' and '&' are left in place.
        assertEquals(listOf("a/b-&-c"), slugsOf("A/B & C"))
    }

    @Test
    fun `BUG an all-symbol label is NOT emptied, so it does not hit the item fallback`() {
        // Nothing is dropped, so the base is non-empty and passes through unchanged.
        assertEquals(listOf("!@#\$%^&*()"), slugsOf("!@#\$%^&*()"))
    }

    @Test
    fun `BUG a double space produces a double dash`() {
        // "  " → "--": both spaces are replaced, and there is no trim to collapse the ends.
        assertEquals(listOf("--"), slugsOf("  "))
    }

    // ---- empty-base fallback ------------------------------------------------

    @Test
    fun `a genuinely empty label falls back to item`() {
        assertEquals(listOf("item"), slugsOf(""))
    }

    // ---- whole-list dedupe + order ------------------------------------------

    @Test
    fun `a duplicate label gets a -2 suffix, keeping the first as the base`() {
        assertEquals(listOf("players", "players-2"), slugsOf("Players", "Players"))
    }

    @Test
    fun `three colliding labels number -2 then -3`() {
        assertEquals(listOf("players", "players-2", "players-3"), slugsOf("Players", "Players", "Players"))
    }

    @Test
    fun `input order is preserved (slugify does not re-sort)`() {
        // slugify consumes the ALREADY-sorted list; it must not reorder — the sort is resolve()'s job.
        assertEquals(listOf("zebra", "alpha"), slugsOf("Zebra", "Alpha"))
    }

    @Test
    fun `all produced slugs are unique`() {
        val slugs = slugsOf("Players", "Players", "Game Content", "")
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
