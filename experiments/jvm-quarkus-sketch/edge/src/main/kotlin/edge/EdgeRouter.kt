package edge

import java.util.concurrent.ConcurrentHashMap

/**
 * A handler at the byte edge: request payload bytes in, reply payload bytes out. Most handlers are
 * written typed via [typedHandler]; this raw form is what the router stores and invokes.
 */
fun interface EdgeHandler {
    fun handle(payload: ByteArray): ByteArray
}

/**
 * The transport-agnostic dispatch core: a method-name → handler table. [dispatch] runs the handler
 * and wraps the result in a [Response] carrying the request's [Request.cid]; a handler throw (or an
 * unknown method) becomes `Response(ok=false, error=…)` instead of propagating. No I/O here — the
 * router turns a Request into a Response; the transport moves the bytes.
 *
 * Two resolution tiers: an EXACT method→handler table (the original behaviour, always wins) and a
 * PREFIX table ([registerPrefix]) consulted only on an exact-match miss. The prefix tier is what lets
 * a gateway route a whole family of methods (`characters.*`) to one forwarding handler without naming
 * each method; an exact registration still shadows any prefix, and an unknown method matching NO
 * prefix still yields the same "no such method" error.
 */
class EdgeRouter {
    private val handlers = ConcurrentHashMap<String, EdgeHandler>()
    private val prefixHandlers = ConcurrentHashMap<String, EdgeHandler>()

    fun register(method: String, handler: EdgeHandler) {
        handlers[method] = handler
    }

    /**
     * Registers a handler for every method starting with [prefix]. Consulted AFTER the exact table
     * misses; when several prefixes match, the LONGEST wins (most specific), so overlapping prefixes
     * like `characters.` and `characters.admin.` resolve deterministically.
     */
    fun registerPrefix(prefix: String, handler: EdgeHandler) {
        prefixHandlers[prefix] = handler
    }

    @Suppress("TooGenericExceptionCaught") // deliberate: `handler` is arbitrary caller-registered
    // code (see class doc — "a handler throw ... becomes Response(ok=false...)"), so the dispatch
    // loop must convert ANY handler failure to an error Response rather than propagating it.
    fun dispatch(req: Request): Response {
        val handler = handlers[req.method] ?: longestPrefixHandler(req.method)
            ?: return Response(req.cid, ok = false, payload = ByteArray(0), error = "no such method: ${req.method}")
        return try {
            Response(req.cid, ok = true, payload = handler.handle(req.payload))
        } catch (e: Exception) {
            Response(req.cid, ok = false, payload = ByteArray(0), error = e.message ?: e.toString())
        }
    }

    /** The registered prefix that [method] starts with and is the longest such prefix; null if none match. */
    private fun longestPrefixHandler(method: String): EdgeHandler? =
        prefixHandlers.entries
            .filter { method.startsWith(it.key) }
            .maxByOrNull { it.key.length }
            ?.value
}
