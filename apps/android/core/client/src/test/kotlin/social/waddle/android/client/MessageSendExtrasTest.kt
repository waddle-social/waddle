package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import social.waddle.android.client.prefs.SharedFileRef
import social.waddle.client.ffi.WaddleReferenceType
import social.waddle.client.ffi.WaddleStickerHash

class MessageSendExtrasTest {
    @Test
    fun `reply prefixes the quoted fallback and marks its codepoint range`() {
        val (body, options) = preparedSend(
            stanzaId = "sid-1",
            body = "sounds good",
            extras = MessageSendExtras(
                replyToId = "s1",
                replyToAuthorJid = "room@muc.waddle.test/alice",
                replyParentBody = "line one\nline two",
            ),
        )

        assertEquals("> line one\n> line two\n\nsounds good", body)
        assertEquals("s1", options.reply?.messageId)
        assertEquals("room@muc.waddle.test/alice", options.reply?.authorJid)
        assertEquals(0u, options.fallback?.start)
        assertEquals("> line one\n> line two\n\n".length.toUInt(), options.fallback?.end)
        assertEquals("sid-1", options.stanzaId)
    }

    @Test
    fun `fallback range counts codepoints not utf-16 units`() {
        val parent = "🎉🎉" // two supplementary-plane codepoints, four UTF-16 units
        val (body, options) = preparedSend(
            stanzaId = "sid-1",
            body = "nice",
            extras = MessageSendExtras(
                replyToId = "s1",
                replyToAuthorJid = "alice@waddle.test",
                replyParentBody = parent,
            ),
        )

        // "> 🎉🎉\n\n" = 1('>')+1(' ')+2(emoji)+2(newlines) = 6 codepoints.
        assertEquals(6u, options.fallback?.end)
        assertEquals("> $parent\n\n", body.substringBefore("nice"))
    }

    @Test
    fun `thread target rides along without touching the body`() {
        val (body, options) = preparedSend(
            stanzaId = "sid-1",
            body = "in thread",
            extras = MessageSendExtras(threadId = "t1", threadParent = null),
        )

        assertEquals("in thread", body)
        assertEquals("t1", options.thread?.id)
        assertNull(options.reply)
        assertNull(options.fallback)
    }

    @Test
    fun `strip round-trips the built fallback`() {
        val parent = "hello 🎉 world"
        val prefix = buildReplyFallbackPrefix(parent)
        val wire = prefix + "the reply"

        val stripped = stripReplyFallback(
            wire,
            start = 0u,
            end = prefix.codePointCount(0, prefix.length).toUInt(),
        )

        assertEquals("the reply", stripped)
    }

    @Test
    fun `invalid fallback ranges leave the body untouched`() {
        assertEquals("abc", stripReplyFallback("abc", 2u, 1u))
        assertEquals("abc", stripReplyFallback("abc", 0u, 99u))
        assertEquals("abc", stripReplyFallback("abc", null, 2u))
    }

    @Test
    fun `mentions become typed mention references on the send options`() {
        val (body, options) = preparedSend(
            stanzaId = "sid-1",
            body = "hi @bob",
            extras = MessageSendExtras(
                mentions = listOf(MentionRef(uri = "xmpp:bob@waddle.test", begin = 3u, end = 7u)),
            ),
        )

        assertEquals("hi @bob", body)
        assertEquals(1, options.references.size)
        val reference = options.references.single()
        assertEquals(WaddleReferenceType.Mention, reference.refType)
        assertEquals("xmpp:bob@waddle.test", reference.uri)
        assertEquals(3u, reference.begin)
        assertEquals(7u, reference.end)
        assertNull(reference.anchor)
    }

    @Test
    fun `mention offsets are codepoints and round-trip past an emoji`() {
        // "😀 @bob": the emoji is ONE code point (two UTF-16 units), so
        // the mention starts at code point 2 — slicing the final body by
        // code points must recover the label exactly.
        val body = "😀 @bob"
        val begin = body.codePointCount(0, body.indexOf("@bob")).toUInt()
        val end = begin + "@bob".codePointCount(0, "@bob".length).toUInt()

        val (wireBody, options) = preparedSend(
            stanzaId = "sid-1",
            body = body,
            extras = MessageSendExtras(
                mentions = listOf(MentionRef(uri = "xmpp:bob@waddle.test", begin = begin, end = end)),
            ),
        )

        val reference = options.references.single()
        assertEquals(2u, reference.begin)
        assertEquals(6u, reference.end)
        val start = wireBody.offsetByCodePoints(0, reference.begin.toInt())
        val stop = wireBody.offsetByCodePoints(0, reference.end.toInt())
        assertEquals("@bob", wireBody.substring(start, stop))
    }

    @Test
    fun `mention offsets shift right past the reply fallback prefix`() {
        val (wireBody, options) = preparedSend(
            stanzaId = "sid-1",
            body = "hi @bob",
            extras = MessageSendExtras(
                replyToId = "s1",
                replyToAuthorJid = "room@muc.waddle.test/alice",
                replyParentBody = "parent 🎉",
                mentions = listOf(MentionRef(uri = "xmpp:bob@waddle.test", begin = 3u, end = 7u)),
            ),
        )

        val reference = options.references.single()
        assertEquals(checkNotNull(options.fallback).end + 3u, reference.begin)
        assertEquals(checkNotNull(options.fallback).end + 7u, reference.end)
        val start = wireBody.offsetByCodePoints(0, reference.begin.toInt())
        val stop = wireBody.offsetByCodePoints(0, reference.end.toInt())
        assertEquals("@bob", wireBody.substring(start, stop))
    }

    @Test
    fun `sticker maps to the pack ref and an inline shared file with desc and hashes`() {
        val (body, options) = preparedSend(
            stanzaId = "sid-1",
            body = "🐧",
            extras = MessageSendExtras(
                sticker = StickerSendRef(
                    packId = "pack-1",
                    desc = "🐧",
                    url = "https://upload.waddle.test/penguin.webp",
                    mediaType = "image/webp",
                    sizeBytes = 1234L,
                    width = 512,
                    height = 256,
                    hashes = listOf(StickerHash(algo = "sha-256", valueB64 = "aGFzaA==")),
                ),
            ),
        )

        assertEquals("🐧", body)
        assertEquals("pack-1", options.sticker?.packId)
        assertNull("own PEP packs carry no external locator", options.sticker?.packJid)
        assertNull(options.sticker?.packNode)
        val file = options.sharedFiles.single()
        assertEquals("https://upload.waddle.test/penguin.webp", file.url)
        assertEquals("🐧", file.desc)
        assertEquals("image/webp", file.mediaType)
        assertEquals(1234uL, file.size)
        assertEquals(512u, file.width)
        assertEquals(256u, file.height)
        assertEquals(FileDisposition.INLINE.wire, file.disposition)
        assertEquals(listOf(WaddleStickerHash(algo = "sha-256", valueB64 = "aGFzaA==")), file.hashes)
        assertNull(file.encrypted)
    }

    @Test
    fun `sticker file appends after regular shared files`() {
        val (_, options) = preparedSend(
            stanzaId = "sid-1",
            body = "🐧",
            extras = MessageSendExtras(
                sharedFiles = listOf(SharedFileRef(url = "https://upload.waddle.test/doc.pdf")),
                sticker = StickerSendRef(
                    packId = "pack-1",
                    desc = "🐧",
                    url = "https://upload.waddle.test/penguin.webp",
                ),
            ),
        )

        assertEquals(
            listOf("https://upload.waddle.test/doc.pdf", "https://upload.waddle.test/penguin.webp"),
            options.sharedFiles.map { it.url },
        )
        assertEquals("pack-1", options.sticker?.packId)
    }

    @Test
    fun `empty mention spans are dropped instead of emitting the sentinel`() {
        val (_, options) = preparedSend(
            stanzaId = "sid-1",
            body = "hi",
            extras = MessageSendExtras(
                mentions = listOf(MentionRef(uri = "xmpp:bob@waddle.test", begin = 0u, end = 0u)),
            ),
        )

        assertEquals(emptyList<Any>(), options.references)
    }
}
