package admin

import admin.adminapi.AdminSection
import admin.adminapi.SectionData
import com.sun.net.httpserver.HttpExchange
import com.sun.net.httpserver.HttpServer
import core.Context
import core.Module
import core.Starter
import core.Stopper
import freemarker.template.Configuration
import freemarker.template.TemplateExceptionHandler
import java.io.StringWriter
import java.net.InetSocketAddress
import java.nio.charset.StandardCharsets
import java.util.Base64
import java.util.concurrent.Executors

/**
 * Serves the GameOps admin console at /admin. Owns the LOOK (theme.css + admin.ftl, both on the
 * classpath) and composes a navigable sidebar from the items modules CONTRIBUTE to [AdminSection]:
 * items are grouped by section, `/admin/<slug>` renders one item, `/admin` redirects to the first.
 * It never imports a module's implementation. A module appears by contributing an Item.
 */
class AdminModule : Module, Starter, Stopper {
    override val name = "admin"

    private lateinit var ctx: Context
    private var server: HttpServer? = null

    private val fm = Configuration(Configuration.VERSION_2_3_34).apply {
        setClassLoaderForTemplateLoading(AdminModule::class.java.classLoader, "templates")
        defaultEncoding = "UTF-8"
        templateExceptionHandler = TemplateExceptionHandler.RETHROW_HANDLER
    }

    override fun init(ctx: Context) {
        this.ctx = ctx
    }

    override fun start(ctx: Context) {
        val port = (System.getenv("ADMIN_PORT") ?: "8090").toInt()
        val srv = HttpServer.create(InetSocketAddress(port), 0)
        srv.createContext("/admin/theme.css") { ex -> serveStatic(ex, "static/theme.css", "text/css") }
        srv.createContext("/admin") { ex -> serveDashboard(ex) }
        srv.executor = Executors.newVirtualThreadPerTaskExecutor()
        srv.start()
        server = srv
        val gate = if (System.getenv("ADMIN_USER") != null) "(HTTP Basic on)"
                   else "(OPEN — set ADMIN_USER/ADMIN_PASS to gate)"
        println("admin on http://localhost:$port/admin  $gate")
    }

    override fun stop() {
        server?.stop(0)
    }

    /** section, label, slug and live renderer for one contributed item (unique slug). */
    private class Resolved(val section: String, val label: String, val slug: String, val render: () -> SectionData)

    /** Type-assert the contributions, slugify labels, and DEDUPE (players, players-2, …). Order kept. */
    private fun resolve(): List<Resolved> {
        val seen = HashSet<String>()
        return ctx.contributions(AdminSection).map { it ->
            val base = slug(it.label).ifEmpty { "item" }
            var s = base
            var n = 2
            while (!seen.add(s)) { s = "$base-$n"; n++ }
            Resolved(it.section, it.label, s, it.render)
        }
    }

    private fun serveDashboard(ex: HttpExchange) {
        if (!authorized(ex)) {
            ex.responseHeaders.add("WWW-Authenticate", "Basic realm=\"admin\"")
            ex.sendResponseHeaders(401, -1); ex.close(); return
        }
        val items = resolve()
        val requested = ex.requestURI.path.removePrefix("/admin").trim('/')

        if (requested.isEmpty()) {                       // bare /admin -> first item, or empty state
            if (items.isEmpty()) { render(ex, emptyModel()); return }
            ex.responseHeaders.add("Location", "/admin/${items.first().slug}")
            ex.sendResponseHeaders(302, -1); ex.close(); return
        }

        val current = items.firstOrNull { it.slug == requested }
        if (current == null) { ex.sendResponseHeaders(404, -1); ex.close(); return }

        // Sidebar: group items by section, first-seen order (LinkedHashMap preserves insertion order).
        val groups = LinkedHashMap<String, MutableList<Map<String, Any?>>>()
        for (it in items) {
            groups.getOrPut(it.section) { mutableListOf() }
                .add(mapOf("label" to it.label, "slug" to it.slug, "active" to (it.slug == current.slug)))
        }
        // Content: render the current item LIVE; on failure show an error state, not a blank page.
        val page: Map<String, Any?> = try {
            val data = current.render()
            mapOf("title" to current.label, "kpis" to data.kpis, "table" to data.table, "err" to null)
        } catch (e: Exception) {
            System.err.println("admin render failed for '${current.label}': $e")
            mapOf("title" to current.label, "err" to "failed to load: ${e.message}")
        }

        val model = HashMap<String, Any?>()
        model["groups"] = groups.map { (section, navItems) -> mapOf("section" to section, "items" to navItems) }
        model["crumb"] = current.section
        model["title"] = current.label
        model["page"] = page
        render(ex, model)
    }

    private fun emptyModel(): Map<String, Any?> =
        mapOf("groups" to emptyList<Any?>(), "crumb" to "Admin", "title" to "Admin", "page" to null)

    private fun render(ex: HttpExchange, model: Map<String, Any?>) {
        val out = StringWriter()
        fm.getTemplate("admin.ftl").process(model, out)
        respond(ex, 200, out.toString().toByteArray(StandardCharsets.UTF_8), "text/html; charset=utf-8")
    }

    private fun slug(s: String) = s.lowercase().replace(" ", "-")

    private fun serveStatic(ex: HttpExchange, resource: String, contentType: String) {
        val bytes = AdminModule::class.java.classLoader.getResourceAsStream(resource)?.use { it.readBytes() }
        if (bytes == null) { ex.sendResponseHeaders(404, -1); ex.close(); return }
        respond(ex, 200, bytes, contentType)
    }

    private fun respond(ex: HttpExchange, code: Int, body: ByteArray, contentType: String) {
        ex.responseHeaders.add("Content-Type", contentType)
        ex.sendResponseHeaders(code, body.size.toLong())
        ex.responseBody.use { it.write(body) }
    }

    /** HTTP Basic gate. Unset ADMIN_USER = open (local only). */
    private fun authorized(ex: HttpExchange): Boolean {
        val user = System.getenv("ADMIN_USER") ?: return true
        val pass = System.getenv("ADMIN_PASS") ?: ""
        val header = ex.requestHeaders.getFirst("Authorization") ?: return false
        if (!header.startsWith("Basic ")) return false
        val decoded = String(Base64.getDecoder().decode(header.removePrefix("Basic ")))
        return decoded == "$user:$pass"
    }
}
