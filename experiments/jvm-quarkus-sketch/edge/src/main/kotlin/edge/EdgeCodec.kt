package edge

import com.fasterxml.jackson.databind.ObjectMapper
import com.fasterxml.jackson.module.kotlin.registerKotlinModule
import org.msgpack.jackson.dataformat.MessagePackFactory

/**
 * The wire discriminator. Jackson-msgpack encodes an enum as its name string — one obvious tag that
 * round-trips cleanly. Kept out of [EdgeMessage] so the public model stays clean; it only exists on
 * the flat [Frame] the codec serializes.
 */
enum class Kind { REQUEST, RESPONSE, PUSH }

/**
 * The flat, dead-simple on-wire shape. Every [EdgeMessage] maps to one [Frame]; a single [kind] tag
 * plus the union of all three kinds' fields (nulls where a kind doesn't use them). Jackson-msgpack
 * serializes this by reflection over the data class — no schema, no codegen.
 */
data class Frame(
    val kind: Kind,
    val cid: Long,
    val method: String? = null,
    val topic: String? = null,
    val ok: Boolean = false,
    val error: String? = null,
    val payload: ByteArray = ByteArray(0),
)

/**
 * Encodes/decodes the whole [EdgeMessage] envelope AND typed payload objects with a single
 * MessagePack [ObjectMapper]. `payload` bytes are themselves MessagePack blobs of the method's own
 * request/response object, so [typedHandler] lets handlers speak typed Kotlin, not bytes.
 *
 * Transport-agnostic: the codec knows nothing about queues, sockets, or QUIC — it turns messages
 * into `ByteArray` and back. The transport moves those bytes.
 */
class EdgeCodec(val mapper: ObjectMapper = defaultMapper()) {

    fun encode(msg: EdgeMessage): ByteArray = mapper.writeValueAsBytes(msg.toFrame())

    fun decode(bytes: ByteArray): EdgeMessage = mapper.readValue(bytes, Frame::class.java).toMessage()

    /** MessagePack-encode a typed payload object (a method's request or reply). */
    fun encodePayload(obj: Any?): ByteArray = mapper.writeValueAsBytes(obj)

    /** MessagePack-decode a payload blob back into its typed object. */
    fun <T> decodePayload(bytes: ByteArray, type: Class<T>): T = mapper.readValue(bytes, type)

    private fun EdgeMessage.toFrame(): Frame = when (this) {
        is Request -> Frame(Kind.REQUEST, cid, method = method, payload = payload)
        is Response -> Frame(Kind.RESPONSE, cid, ok = ok, error = error, payload = payload)
        is Push -> Frame(Kind.PUSH, cid, topic = topic, payload = payload)
    }

    private fun Frame.toMessage(): EdgeMessage = when (kind) {
        Kind.REQUEST -> Request(cid, requireNotNull(method) { "REQUEST frame missing method" }, payload)
        Kind.RESPONSE -> Response(cid, ok, payload, error)
        Kind.PUSH -> Push(cid, requireNotNull(topic) { "PUSH frame missing topic" }, payload)
    }

    companion object {
        /** An ObjectMapper over MessagePack with the Kotlin module — data classes in, msgpack out. */
        fun defaultMapper(): ObjectMapper = ObjectMapper(MessagePackFactory()).registerKotlinModule()
    }
}

/**
 * Turns a typed function `(Req) -> Resp` into an [EdgeHandler] that speaks the wire: msgpack-decode
 * the request payload, run the function, msgpack-encode the reply. The router never sees the types.
 */
inline fun <reified Req, Resp> EdgeCodec.typedHandler(crossinline fn: (Req) -> Resp): EdgeHandler =
    EdgeHandler { payload -> encodePayload(fn(decodePayload(payload, Req::class.java))) }
