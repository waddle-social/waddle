package social.waddle.android.client

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Test
import social.waddle.android.client.prefs.SharedFileRef

class FileDispositionTest {
    /** Same configuration as `SessionPrefs`' outbound-queue codec. */
    private val json = Json { ignoreUnknownKeys = true }

    @Test
    fun `media types map to web inferredFileDisposition parity`() {
        assertEquals(FileDisposition.INLINE, FileDisposition.forMediaType("image/png"))
        assertEquals(FileDisposition.INLINE, FileDisposition.forMediaType("video/mp4"))
        assertEquals(FileDisposition.INLINE, FileDisposition.forMediaType("audio/ogg"))
        assertEquals(FileDisposition.INLINE, FileDisposition.forMediaType("application/pdf"))
        assertEquals(FileDisposition.ATTACHMENT, FileDisposition.forMediaType("application/zip"))
        assertEquals(FileDisposition.ATTACHMENT, FileDisposition.forMediaType("text/plain"))
        assertEquals(FileDisposition.ATTACHMENT, FileDisposition.forMediaType(null))
    }

    @Test
    fun `wire strings round-trip and unknown values degrade to attachment`() {
        assertEquals("inline", FileDisposition.INLINE.wire)
        assertEquals("attachment", FileDisposition.ATTACHMENT.wire)
        assertEquals(FileDisposition.INLINE, FileDisposition.fromWire("inline"))
        assertEquals(FileDisposition.ATTACHMENT, FileDisposition.fromWire("attachment"))
        assertEquals(FileDisposition.ATTACHMENT, FileDisposition.fromWire("banner"))
        assertEquals(FileDisposition.ATTACHMENT, FileDisposition.fromWire(null))
    }

    @Test
    fun `persisted queue json is byte-identical to the raw string field`() {
        val ref = SharedFileRef(
            url = "https://files.waddle.test/cat.png",
            name = "cat.png",
            mediaType = "image/png",
            sizeBytes = 42,
            disposition = FileDisposition.INLINE,
        )

        val encoded = json.encodeToString(SharedFileRef.serializer(), ref)

        // Exactly what the previous `disposition: String = "attachment"`
        // field produced for an inline file.
        assertEquals(
            """{"url":"https://files.waddle.test/cat.png","name":"cat.png",""" +
                """"mediaType":"image/png","sizeBytes":42,"disposition":"inline"}""",
            encoded,
        )
        assertEquals(ref, json.decodeFromString(SharedFileRef.serializer(), encoded))
    }

    @Test
    fun `legacy queue entries without a disposition decode as attachment`() {
        val decoded = json.decodeFromString(
            SharedFileRef.serializer(),
            """{"url":"https://files.waddle.test/notes.zip"}""",
        )

        assertEquals(FileDisposition.ATTACHMENT, decoded.disposition)
        // The default is omitted on re-encode, exactly like the old
        // String default was.
        assertEquals(
            """{"url":"https://files.waddle.test/notes.zip"}""",
            json.encodeToString(SharedFileRef.serializer(), decoded),
        )
    }
}
