package admin.adminapi

/**
 * The admin contract — the ONLY thing a module imports to appear in the admin portal.
 *
 * vs the CDI-closure design (Step 1–5): admin used to aggregate `@All List<Item>` where each
 * [Item] carried a NON-serializable `render: () -> SectionData` closure — fundamentally in-process,
 * so a module living in another JVM could never contribute. Step 6 splits that seam into two halves
 * so admin can fan out over HTTP:
 *  - [AdminDataProvider] — a LOCAL, CDI-discovered capability (`@All List<AdminDataProvider>`). A
 *    module in THIS process implements it; `data()` reads its live DB.
 *  - [AdminItemDto] — the WIRE shape. A module also exposes `/admin-data/<id>` returning this DTO,
 *    so admin can fetch a REMOTE module's dashboard over REST/JSON exactly as if it were local.
 *
 * [SectionData]/[Kpi]/[Table]/[Cell] are plain serializable data classes (Jackson-friendly: no
 * functions, all fields concrete with defaults) shared by both halves.
 */

/** A single headline number. */
data class Kpi(val label: String, val value: String)

/** One table cell. `mono` = monospace (ids), `badge` = pill styling. */
data class Cell(val text: String, val mono: Boolean = false, val badge: Boolean = false)

data class Table(val headers: List<String>, val rows: List<List<Cell>>)

/** The live dashboard payload for one module — serializable, so it crosses the wire unchanged. */
data class SectionData(val kpis: List<Kpi> = emptyList(), val table: Table? = null)

/**
 * A module's admin dashboard, produced as a CDI bean in the SAME process as the module. Admin
 * injects `@All List<AdminDataProvider>` and, for a locally-hosted module, calls [data] in-process.
 * The bean TYPE is the contribution slot — a new local contributor appears with zero edits to admin.
 */
interface AdminDataProvider {
    val id: String
    val section: String
    val label: String
    fun data(): SectionData
}

/**
 * The wire shape of one module's dashboard: the [AdminDataProvider] fields plus its [SectionData].
 * A module publishes it at `/admin-data/<id>`; admin fetches it over REST when the module is remote,
 * or builds it from the local provider when co-located. Either way the admin renders this same DTO.
 */
data class AdminItemDto(
    val id: String,
    val section: String,
    val label: String,
    val data: SectionData,
)
