package admin.adminapi

import core.Slot

/**
 * The admin contract — the ONLY thing a module imports to appear in the admin portal. It depends on
 * this package (declarative widgets + the slot), never on the `admin` implementation. The admin
 * owns rendering; a contributor just returns data. A new contributor appears with no edit to admin.
 */

/** A single headline number. */
data class Kpi(val label: String, val value: String)

/** One table cell. `mono` = monospace (ids), `badge` = pill styling. */
data class Cell(val text: String, val mono: Boolean = false, val badge: Boolean = false)

data class Table(val headers: List<String>, val rows: List<List<Cell>>)

/** What an item renders at request time (live data). */
data class SectionData(val kpis: List<Kpi> = emptyList(), val table: Table? = null)

/**
 * A clickable entry in the admin sidebar, contributed by a module. The admin groups items by
 * [section] into the menu; opening an item renders [render] into the content area. `render` runs
 * per request, so the numbers are always live.
 */
class Item(val section: String, val label: String, val render: () -> SectionData)

/** The slot modules contribute items to; the admin reads them all and groups by section. */
val AdminSection = Slot<Item>("admin.item")
