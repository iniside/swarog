package edge

import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertInstanceOf
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * Pure-unit tests of [EdgeCodec] / [Frame] — zero transport, zero native. Locks the wire round-trip of
 * all three [EdgeMessage] kinds AND the malformed-input behavior (the private `Frame.toMessage`
 * `requireNotNull` guards, garbage/truncated bytes, and the typed-payload decode mismatch). Payloads
 * are compared with [assertArrayEquals] because [Request]/[Response]/[Push] carry `ByteArray` (data-class
 * equality on those is identity, so whole-message `assertEquals` would be meaningless).
 */
class EdgeCodecTest {

    private val codec = EdgeCodec()

    @Test
    fun `encodes and decodes a Request preserving cid, method and payload`() {
        val payload = byteArrayOf(1, 2, 3, 4)
        val req = assertInstanceOf(Request::class.java, codec.decode(codec.encode(Request(9L, "characters.list", payload))))
        assertEquals(9L, req.cid)
        assertEquals("characters.list", req.method)
        assertArrayEquals(payload, req.payload)
    }

    @Test
    fun `encodes and decodes an ok Response`() {
        val payload = byteArrayOf(5, 6)
        val resp = assertInstanceOf(Response::class.java, codec.decode(codec.encode(Response(3L, ok = true, payload = payload))))
        assertEquals(3L, resp.cid)
        assertTrue(resp.ok)
        assertNull(resp.error)
        assertArrayEquals(payload, resp.payload)
    }

    @Test
    fun `encodes and decodes an error Response with a null error message`() {
        val resp = assertInstanceOf(
            Response::class.java,
            codec.decode(codec.encode(Response(4L, ok = false, payload = ByteArray(0), error = null))),
        )
        assertFalse(resp.ok)
        assertNull(resp.error)
    }

    @Test
    fun `encodes and decodes an error Response with a non-null error message`() {
        val resp = assertInstanceOf(
            Response::class.java,
            codec.decode(codec.encode(Response(5L, ok = false, payload = ByteArray(0), error = "boom"))),
        )
        assertFalse(resp.ok)
        assertEquals("boom", resp.error)
    }

    @Test
    fun `encodes and decodes a Push preserving topic, payload and cid 0`() {
        val payload = byteArrayOf(9, 8, 7)
        val push = assertInstanceOf(Push::class.java, codec.decode(codec.encode(Push(topic = "characters.created", payload = payload))))
        assertEquals(0L, push.cid)
        assertEquals("characters.created", push.topic)
        assertArrayEquals(payload, push.payload)
    }

    @Test
    fun `decoding a REQUEST frame whose method is null throws`() {
        val bytes = codec.mapper.writeValueAsBytes(Frame(Kind.REQUEST, cid = 1L, method = null))
        assertThrows(IllegalArgumentException::class.java) { codec.decode(bytes) }
    }

    @Test
    fun `decoding a PUSH frame whose topic is null throws`() {
        val bytes = codec.mapper.writeValueAsBytes(Frame(Kind.PUSH, cid = 0L, topic = null))
        assertThrows(IllegalArgumentException::class.java) { codec.decode(bytes) }
    }

    @Test
    fun `decoding garbage bytes throws`() {
        // 0xC1 is the one byte MessagePack reserves as "never used" — a guaranteed decode failure.
        assertThrows(Exception::class.java) { codec.decode(byteArrayOf(0xC1.toByte())) }
    }

    @Test
    fun `decoding a truncated frame throws`() {
        val full = codec.encode(Request(1L, "characters.list", byteArrayOf(1, 2, 3, 4, 5, 6)))
        assertThrows(Exception::class.java) { codec.decode(full.copyOf(full.size / 2)) }
    }

    @Test
    fun `a typedHandler rejects a payload that does not match its request type`() {
        val handler = codec.typedHandler<CharactersReply, String> { it.names.first() }
        val wrongPayload = codec.encodePayload(ListCharactersRequest("player-1"))
        assertThrows(Exception::class.java) { handler.handle(wrongPayload) }
    }
}
