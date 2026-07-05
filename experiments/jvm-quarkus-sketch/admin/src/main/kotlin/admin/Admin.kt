package admin

import admin.adminapi.Item
import admin.adminapi.Kpi
import admin.adminapi.SectionData
import admin.adminapi.Table
import io.quarkus.arc.All
import io.quarkus.qute.Location
import io.quarkus.qute.Template
import jakarta.ws.rs.GET
import jakarta.ws.rs.Path
import jakarta.ws.rs.PathParam
import jakarta.ws.rs.core.Context
import jakarta.ws.rs.core.HttpHeaders
import jakarta.ws.rs.core.Response
import java.net.URI
import java.util.Base64

/**
 * Serves the GameOps admin console at /admin. Owns the LOOK (theme.css served by Quarkus'
 * static-resource handler from META-INF/resources, admin.html rendered by Qute) and composes
 * the sidebar from every [Item] bean in the container (`@All List<Item>`) — it never imports
 * a module's implementation. A module appears by producing an Item bean.
 *
 * vs the framework-free sketch: no HttpServer, no Starter/Stopper, no handler wiring — the
 * container owns the HTTP lifecycle and this class is just routes.
 */
@Path("/admin")
class AdminResource(
    @All private val contributed: MutableList<Item>,
    @Location("admin.html") private val template: Template,
) {

    /** section, label, slug and live renderer for one contributed item (unique slug). */
    class Resolved(val section: String, val label: String, val slug: String, val render: () -> SectionData)

    /** view model for the sidebar */
    class NavItem(val label: String, val slug: String, val active: Boolean)
    class NavGroup(val section: String, val items: List<NavItem>)
    class Page(val title: String, val kpis: List<Kpi> = emptyList(), val table: Table? = null, val err: String? = null)

    /** Slugify labels and DEDUPE (players, players-2, …). `@All` discovery order is container-
     *  defined and modules must not care — so the ADMIN imposes a deterministic presentation
     *  order (section, then label). Rendering, including ordering, is the renderer's job. */
    private fun resolve(): List<Resolved> {
        val seen = HashSet<String>()
        return contributed.sortedWith(compareBy({ it.section }, { it.label })).map {
            val base = it.label.lowercase().replace(" ", "-").ifEmpty { "item" }
            var s = base
            var n = 2
            while (!seen.add(s)) { s = "$base-$n"; n++ }
            Resolved(it.section, it.label, s, it.render)
        }
    }

    @GET
    fun root(@Context headers: HttpHeaders): Response {
        unauthorized(headers)?.let { return it }
        val items = resolve()
        if (items.isEmpty()) {
            return html(template.data("groups", emptyList<NavGroup>(), "crumb", "Admin", "title", "Admin", "page", null))
        }
        return Response.seeOther(URI.create("/admin/${items.first().slug}")).build()
    }

    @GET
    @Path("{slug}")
    fun page(@PathParam("slug") slug: String, @Context headers: HttpHeaders): Response {
        unauthorized(headers)?.let { return it }
        val items = resolve()
        val current = items.firstOrNull { it.slug == slug }
            ?: return Response.status(404).build()

        // Sidebar: group items by section, first-seen order.
        val groups = items.groupBy { it.section }.map { (section, list) ->
            NavGroup(section, list.map { NavItem(it.label, it.slug, it.slug == current.slug) })
        }
        // Content: render the current item LIVE; on failure show an error state, not a blank page.
        val page = try {
            val data = current.render()
            Page(title = current.label, kpis = data.kpis, table = data.table)
        } catch (e: Exception) {
            System.err.println("admin render failed for '${current.label}': $e")
            Page(title = current.label, err = "failed to load: ${e.message}")
        }
        return html(template.data("groups", groups, "crumb", current.section, "title", current.label, "page", page))
    }

    private fun html(instance: io.quarkus.qute.TemplateInstance): Response =
        Response.ok(instance.render(), "text/html; charset=utf-8").build()

    /** HTTP Basic gate. Unset ADMIN_USER = open (local only). Returns a 401 response or null. */
    private fun unauthorized(headers: HttpHeaders): Response? {
        val user = System.getenv("ADMIN_USER") ?: return null
        val pass = System.getenv("ADMIN_PASS") ?: ""
        val header = headers.getHeaderString("Authorization")
        if (header != null && header.startsWith("Basic ")) {
            val decoded = String(Base64.getDecoder().decode(header.removePrefix("Basic ")))
            if (decoded == "$user:$pass") return null
        }
        return Response.status(401).header("WWW-Authenticate", "Basic realm=\"admin\"").build()
    }
}
