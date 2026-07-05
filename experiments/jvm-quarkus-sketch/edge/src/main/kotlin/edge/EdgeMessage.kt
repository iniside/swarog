package edge

/**
 * The wire envelope of the client-edge protocol — a tiny sealed model that rides ONE bidirectional
 * byte-stream (today a loopback queue pair; later a QUIC stream). Three kinds:
 *
 *  - [Request]  client → server, correlation id [cid] links the eventual [Response] back to it.
 *  - [Response] server → client, answers a Request (same [cid]); `ok=false` carries an [error].
 *  - [Push]     server → client, UNSOLICITED (cid 0) — the server-push QUIC gives you a stream for
 *               but no request/response framing over.
 *
 * `payload` is itself a MessagePack blob (the method's own request/response object), so handlers
 * deal in typed Kotlin objects at their edge, never raw bytes — see [EdgeCodec.typedHandler].
 */
sealed interface EdgeMessage {
    /** Correlation id. Non-zero on Request/Response (pairs them); 0 on an unsolicited Push. */
    val cid: Long
}

data class Request(
    override val cid: Long,
    val method: String,
    val payload: ByteArray,
) : EdgeMessage

data class Response(
    override val cid: Long,
    val ok: Boolean,
    val payload: ByteArray,
    val error: String? = null,
) : EdgeMessage

data class Push(
    override val cid: Long = 0,
    val topic: String,
    val payload: ByteArray,
) : EdgeMessage
