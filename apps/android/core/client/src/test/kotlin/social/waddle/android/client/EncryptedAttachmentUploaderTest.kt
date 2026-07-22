package social.waddle.android.client

import kotlinx.coroutines.test.runTest
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.OkHttpClient
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import social.waddle.client.ffi.WaddleEncryptedFile
import social.waddle.client.ffi.WaddleEncryptedFileHash
import social.waddle.client.ffi.WaddleUploadSlot
import java.io.ByteArrayInputStream

class EncryptedAttachmentUploaderTest {
    @get:Rule
    val tempFolder = TemporaryFolder()

    private lateinit var server: MockWebServer

    private val plaintext = "confidential penguin memo".toByteArray()

    private class RecordedSlotRequest(
        val filename: String,
        val sizeBytes: ULong,
        val contentType: String,
    )

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
    }

    @After
    fun tearDown() {
        server.close()
    }

    private fun uploader(
        recorded: MutableList<RecordedSlotRequest>,
        grantSlot: Boolean = true,
    ) = EncryptedAttachmentUploader(
        httpClient = OkHttpClient(),
        requestSlot = { filename, sizeBytes, contentType ->
            recorded += RecordedSlotRequest(filename, sizeBytes, contentType)
            if (grantSlot) {
                WaddleUploadSlot(
                    putUrl = server.url("/upload/abc/report.pdf.enc").toString(),
                    getUrl = "https://files.waddle.test/abc/report.pdf.enc",
                    putHeaders = emptyList(),
                )
            } else {
                null
            }
        },
        tempDir = tempFolder.root,
    )

    @Test
    fun `requests a slot for the ciphertext under the enc name and uploads decryptable bytes`() = runTest {
        server.enqueue(MockResponse(code = 201))
        val recorded = mutableListOf<RecordedSlotRequest>()

        val result = uploader(recorded).upload(
            name = "report.pdf",
            declaredSize = plaintext.size.toLong(),
            mediaType = "application/pdf",
        ) { ByteArrayInputStream(plaintext) }

        // Slot: `<name>.enc`, octet-stream, CIPHERTEXT length (+16 tag).
        val slotRequest = recorded.single()
        assertEquals("report.pdf.enc", slotRequest.filename)
        assertEquals("application/octet-stream", slotRequest.contentType)
        assertEquals(
            (plaintext.size + EncryptedAttachmentCrypto.GCM_TAG_BYTES).toULong(),
            slotRequest.sizeBytes,
        )

        val done = result as UploadResult.Done
        val ref = done.file
        assertEquals("https://files.waddle.test/abc/report.pdf.enc", ref.url)
        assertEquals("report.pdf", ref.name)
        assertEquals("application/pdf", ref.mediaType)
        assertEquals(plaintext.size.toLong(), ref.sizeBytes)
        assertEquals(FileDisposition.INLINE, ref.disposition)
        val encrypted = checkNotNull(ref.encrypted)
        assertEquals(listOf(ref.url), encrypted.sources)
        assertEquals(EncryptedAttachmentCrypto.CIPHER_AES_256_GCM, encrypted.cipher)
        assertTrue(ref.hashes.isNotEmpty())
        assertTrue(encrypted.hashes.isNotEmpty())
        // Plaintext and ciphertext digests digest different bytes.
        assertTrue(ref.hashes.single().valueB64 != encrypted.hashes.single().valueB64)

        // The PUT body is real ciphertext: it decrypts with the envelope.
        val putRequest = server.takeRequest()
        assertEquals("PUT", putRequest.method)
        val putBody = checkNotNull(putRequest.body).toByteArray()
        val decrypted = EncryptedAttachmentCrypto.decrypt(
            ciphertext = putBody,
            encrypted = WaddleEncryptedFile(
                cipher = encrypted.cipher,
                keyB64 = encrypted.keyB64,
                ivB64 = encrypted.ivB64,
                hashes = encrypted.hashes.map {
                    WaddleEncryptedFileHash(algo = it.algo, valueB64 = it.valueB64)
                },
                sources = encrypted.sources,
            ),
            declaredSize = ref.sizeBytes,
        )
        assertArrayEquals(plaintext, decrypted)

        // The staged ciphertext temp file is cleaned up after the upload.
        assertEquals(0, tempFolder.root.listFiles().orEmpty().size)
    }

    @Test
    fun `too large plaintext never encrypts or requests a slot`() = runTest {
        val recorded = mutableListOf<RecordedSlotRequest>()

        val result = uploader(recorded).upload(
            name = "huge.bin",
            declaredSize = EncryptedAttachmentUploader.MAX_UPLOAD_BYTES + 1,
            mediaType = "application/octet-stream",
        ) { ByteArrayInputStream(plaintext) }

        assertEquals(UploadResult.TooLarge, result)
        assertTrue(recorded.isEmpty())
    }

    @Test
    fun `a refused slot fails the upload and cleans the staging file`() = runTest {
        val recorded = mutableListOf<RecordedSlotRequest>()

        val result = uploader(recorded, grantSlot = false).upload(
            name = "report.pdf",
            declaredSize = plaintext.size.toLong(),
            mediaType = "application/pdf",
        ) { ByteArrayInputStream(plaintext) }

        assertEquals(UploadResult.Failed, result)
        assertEquals(0, tempFolder.root.listFiles().orEmpty().size)
    }
}
