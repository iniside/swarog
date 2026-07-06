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
 * Three resolution tiers, tried in order: (1) an EXACT method→handler table (the original behaviour,
 * always wins); (2) a payload-only PREFIX table ([registerPrefix] with an [EdgeHandler]); (3) a
 * method-aware PREFIX table ([registerPrefix] with a [MethodForward]) — the gateway's tier, which sees
 * the inbound method so it can forward a whole family under one registration. Within each prefix tier
 * the LONGEST matching prefix wins (most specific), so overlapping prefixes like `characters.` and
 * `characters.admin.` resolve deterministically. An unknown method matching NO tier still yields the
 * same "no such method" error.
 */
class EdgeRouter {
    /**
     * A method-AWARE prefix handler: unlike [EdgeHandler] (payload only), it also receives the inbound
     * [method] name, so a single registration can byte-relay a whole family of methods to ONE downstream
     * while preserving which method was called. Exactly what a gateway needs — `characters.` maps to one
     * forwarding leg, but `characters.list` and `characters.ownerOf` must reach the downstream under
     * their ORIGINAL names. (A payload-only [EdgeHandler] under a prefix can only forward one fixed
     * method, so it is correct only when a prefix fronts exactly one method.)
     */
    fun interface MethodForward {
        fun forward(method: String, payload: ByteArray): ByteArray
    }

    private val handlers = ConcurrentHashMap<String, EdgeHandler>()
    private val prefixHandlers = ConcurrentHashMap<String, EdgeHandler>()
    private val prefixForwarders = ConcurrentHashMap<String, MethodForward>()

    fun register(method: String, handler: EdgeHandler) {
        handlers[method] = handler
    }

    /**
     * Registers a payload-only handler for every method starting with [prefix]. Consulted AFTER the
     * exact table misses; when several prefixes match, the LONGEST wins (most specific).
     */
    fun registerPrefix(prefix: String, handler: EdgeHandler) {
        prefixHandlers[prefix] = handler
    }

    /**
     * Registers a method-AWARE [MethodForward] for every method starting with [prefix] — the gateway's
     * byte-relay tier, which forwards the ORIGINAL inbound method. Overload resolution picks this over
     * [registerPrefix]`(String, EdgeHandler)` by lambda arity (a `{ method, payload -> … }` two-arg
     * lambda). Consulted after the exact table AND the payload-only prefix table miss.
     */
    fun registerPrefix(prefix: String, forward: MethodForward) {
        prefixForwarders[prefix] = forward
    }

    @Suppress("TooGenericExceptionCaught") // deliberate: `handler` is arbitrary caller-registered
    // code (see class doc — "a handler throw ... becomes Response(ok=false...)"), so the dispatch
    // loop must convert ANY handler failure to an error Response rather than propagating it.
    fun dispatch(req: Request): Response {
        val invoke = bind(req)
            ?: return Response(req.cid, ok = false, payload = ByteArray(0), error = "no such method: ${req.method}")
        return try {
            Response(req.cid, ok = true, payload = invoke())
        } catch (e: Exception) {
            Response(req.cid, ok = false, payload = ByteArray(0), error = e.message ?: e.toString())
        }
    }

    /** Resolves [req] to a zero-arg invocation across the three tiers (exact → prefix handler → prefix
     *  forwarder), binding the payload/method; null if nothing matches. */
    private fun bind(req: Request): (() -> ByteArray)? {
        (handlers[req.method] ?: longestPrefixHandler(req.method))?.let { h -> return { h.handle(req.payload) } }
        longestPrefixForward(req.method)?.let { f -> return { f.forward(req.method, req.payload) } }
        return null
    }

    /** The longest registered payload-only prefix that [method] starts with; null if none match. */
    private fun longestPrefixHandler(method: String): EdgeHandler? =
        prefixHandlers.entries
            .filter { method.startsWith(it.key) }
            .maxByOrNull { it.key.length }
            ?.value

    /** The longest registered method-aware prefix that [method] starts with; null if none match. */
    private fun longestPrefixForward(method: String): MethodForward? =
        prefixForwarders.entries
            .filter { method.startsWith(it.key) }
            .maxByOrNull { it.key.length }
            ?.value
}
