package admin.adminapi

/**
 * The admin contract — the ONLY thing a module imports to appear in the admin portal.
 *
 * vs the framework-free sketch: the `Slot<Item>("admin.item")` key is gone. The contribution
 * seam is now CDI itself — a module `@Produces` an [Item] bean, and the admin injects
 * `@All List<Item>` (every Item bean in the container). The bean TYPE is the slot.
 * A new contributor still appears with zero edits to admin.
 */

/** A single headline number. */
data class Kpi(val label: String, val value: String)

/** One table cell. `mono` = monospace (ids), `badge` = pill styling. */
data class Cell(val text: String, val mono: Boolean = false, val badge: Boolean = false)

data class Table(val headers: List<String>, val rows: List<List<Cell>>)

/** What an item renders at request time (live data). */
data class SectionData(val kpis: List<Kpi> = emptyList(), val table: Table? = null)

/**
 * A clickable entry in the admin sidebar, contributed by a module as a CDI bean. The admin
 * groups items by [section]; opening an item renders [render] into the content area.
 * `render` runs per request, so the numbers are always live.
 */
class Item(val section: String, val label: String, val render: () -> SectionData)
