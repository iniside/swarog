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
 */
class EdgeRouter {
    private val handlers = ConcurrentHashMap<String, EdgeHandler>()

    fun register(method: String, handler: EdgeHandler) {
        handlers[method] = handler
    }

    fun dispatch(req: Request): Response {
        val handler = handlers[req.method]
            ?: return Response(req.cid, ok = false, payload = ByteArray(0), error = "no such method: ${req.method}")
        return try {
            Response(req.cid, ok = true, payload = handler.handle(req.payload))
        } catch (e: Exception) {
            Response(req.cid, ok = false, payload = ByteArray(0), error = e.message ?: e.toString())
        }
    }
}
