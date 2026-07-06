package admin

import admin.adminapi.AdminDataProvider
import admin.adminapi.AdminItemDto
import admin.adminapi.Kpi
import admin.adminapi.SectionData
import admin.adminapi.Table
import io.quarkus.arc.All
import io.quarkus.qute.Location
import io.quarkus.qute.Template
import io.quarkus.rest.client.reactive.QuarkusRestClientBuilder
import jakarta.ws.rs.GET
import jakarta.ws.rs.Path
import jakarta.ws.rs.PathParam
import jakarta.ws.rs.core.Context
import jakarta.ws.rs.core.HttpHeaders
import jakarta.ws.rs.core.Response
import java.net.URI
import java.util.Base64
import java.util.concurrent.ConcurrentHashMap
import org.eclipse.microprofile.config.ConfigProvider
import org.eclipse.microprofile.config.inject.ConfigProperty
import platform.RoleConfig

/**
 * Serves the GameOps admin console at /admin. Owns the LOOK (theme.css + admin.html Qute template)
 * and composes the sidebar by FANNING OUT over the `admin.modules` list — for each module either the
 * LOCAL [AdminDataProvider] bean (co-located, called in-process) or a REST fetch of its remote
 * `/admin-data/<id>` endpoint (via `stork://<id>-service`). It still never imports a module's impl:
 * it depends only on the `admin-api` contract ([AdminDataProvider] locally, [AdminItemDto] on the wire).
 *
 * vs the Step 1–5 closure design: `Item.render` was a non-serializable closure aggregated by
 * `@All List<Item>` — strictly in-process. Splitting it into a local provider + a wire DTO lets one
 * admin process render modules that live in OTHER JVMs. Each provider fetch is wrapped in try/catch:
 * a down/booting remote module degrades to an "error card", never a blank /admin.
 */
@Path("/admin")
class AdminResource(
    @All private val localProviders: MutableList<AdminDataProvider>,
    private val roleConfig: RoleConfig,
    @ConfigProperty(name = "admin.modules") private val modules: List<String>,
    @Location("admin.html") private val template: Template,
) {

    /** one composed dashboard (local or remote) + its unique sidebar slug. */
    class Resolved(val dto: AdminItemDto, val slug: String)

    /** view models for the template */
    class NavItem(val label: String, val slug: String, val active: Boolean)
    class NavGroup(val section: String, val items: List<NavItem>)
    class Page(val title: String, val kpis: List<Kpi> = emptyList(), val table: Table? = null, val err: String? = null)

    /** Local providers indexed by id — the co-located modules this process can render in-process. */
    private fun localsById(): Map<String, AdminDataProvider> = localProviders.associateBy { it.id }

    /**
     * Compose every configured module's dashboard. `roleConfig.isActive(id)` ⇒ the module runs in
     * THIS process, so use its local [AdminDataProvider] bean; otherwise fetch it over REST from
     * `admin.<id>.url` (default `stork://<id>-service`). Each fetch is isolated in try/catch → a
     * single unreachable remote becomes an error card, the rest of /admin still renders.
     */
    @Suppress("TooGenericExceptionCaught") // deliberate isolation boundary: `p.data()` is arbitrary
    // module code and `remoteClient(id).fetch(id)` is an arbitrary-failure-mode network call — any
    // single module's failure must degrade to an error card, never blank the whole /admin page.
    private fun fanOut(): List<AdminItemDto> {
        val locals = localsById()
        return modules.map { id ->
            try {
                if (roleConfig.isActive(id)) {
                    val p = locals[id] ?: error("no local AdminDataProvider for active module '$id'")
                    AdminItemDto(p.id, p.section, p.label, p.data())
                } else {
                    remoteClient(id).fetch(id)
                }
            } catch (e: Exception) {
                System.err.println("admin fan-out failed for '$id': $e")
                errorCard(id)
            }
        }
    }

    /** A remote module that is down/booting: show a placeholder card instead of blanking /admin. */
    private fun errorCard(id: String): AdminItemDto = AdminItemDto(
        id = id,
        section = "Game Content",
        label = id.replaceFirstChar { it.uppercase() },
        data = SectionData(kpis = listOf(Kpi("Status", "unavailable"), Kpi("Module", id))),
    )

    /** One programmatic REST client per base URI, cached — `stork://` resolution is wired in Step 7. */
    private fun remoteClient(id: String): AdminDataClient {
        val url = ConfigProvider.getConfig()
            .getOptionalValue("admin.$id.url", String::class.java)
            .orElse("stork://$id-service")
        return clients.computeIfAbsent(url) {
            QuarkusRestClientBuilder.newBuilder().baseUri(URI.create(it)).build(AdminDataClient::class.java)
        }
    }

    /** Fan out, sort by (section, label), then slugify + DEDUPE (inventory, inventory-2, …). Ordering
     *  is the ADMIN's job — remote/local discovery order must not leak into presentation. */
    private fun resolve(): List<Resolved> {
        val seen = HashSet<String>()
        return fanOut().sortedWith(compareBy({ it.section }, { it.label })).map { dto ->
            val base = dto.label.lowercase().replace(" ", "-").ifEmpty { "item" }
            var s = base
            var n = 2
            while (!seen.add(s)) { s = "$base-$n"; n++ }
            Resolved(dto, s)
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

        // Sidebar: group items by section (already sorted, so first-seen == sorted order).
        val groups = items.groupBy { it.dto.section }.map { (section, list) ->
            NavGroup(section, list.map { NavItem(it.dto.label, it.slug, it.slug == current.slug) })
        }
        // Content: the current module's dashboard is already materialized (fanOut handled failures).
        val data = current.dto.data
        val page = Page(title = current.dto.label, kpis = data.kpis, table = data.table)
        return html(template.data("groups", groups, "crumb", current.dto.section, "title", current.dto.label, "page", page))
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

    private companion object {
        /** Base-URI → client cache. Class-level so it survives per-request resource instantiation. */
        private val clients = ConcurrentHashMap<String, AdminDataClient>()
    }
}
