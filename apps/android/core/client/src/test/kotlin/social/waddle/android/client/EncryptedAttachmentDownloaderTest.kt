package social.waddle.android.client

import kotlinx.coroutines.async
import kotlinx.coroutines.test.runTest
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.OkHttpClient
import okio.Buffer
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test
import social.waddle.client.ffi.WaddleEncryptedFile
import social.waddle.client.ffi.WaddleEncryptedFileHash
import java.security.MessageDigest
import java.util.Base64
import java.util.concurrent.TimeUnit
import javax.crypto.Cipher
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

class EncryptedAttachmentDownloaderTest {
    private lateinit var server: MockWebServer
    private lateinit var downloader: EncryptedAttachmentDownloader

    private val plaintext = "penguins waddle in private".toByteArray()
    private val key = ByteArray(32) { (it * 3).toByte() }
    private val iv = ByteArray(12) { (it + 9).toByte() }
    private val ciphertext: ByteArray by lazy {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, SecretKeySpec(key, "AES"), GCMParameterSpec(128, iv))
        cipher.doFinal(plaintext)
    }

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
        downloader = EncryptedAttachmentDownloader(OkHttpClient())
    }

    @After
    fun tearDown() {
        server.close()
    }

    private fun envelope(sources: List<String>, hashValueB64: String? = null) = WaddleEncryptedFile(
        cipher = EncryptedAttachmentCrypto.CIPHER_AES_256_GCM,
        keyB64 = Base64.getEncoder().encodeToString(key),
        ivB64 = Base64.getEncoder().encodeToString(iv),
        hashes = listOfNotNull(
            hashValueB64?.let { WaddleEncryptedFileHash(algo = "sha-256", valueB64 = it) },
        ),
        sources = sources,
    )

    private fun sha256B64(bytes: ByteArray): String =
        Base64.getEncoder().encodeToString(MessageDigest.getInstance("SHA-256").digest(bytes))

    private fun ciphertextResponse() = MockResponse.Builder()
        .code(200)
        .body(Buffer().write(ciphertext))
        .build()

    @Test
    fun `falls back to the next source when the first fails`() = runTest {
        server.enqueue(MockResponse(code = 404))
        server.enqueue(ciphertextResponse())
        val first = server.url("/broken/blob.enc").toString()
        val second = server.url("/mirror/blob.enc").toString()

        val decrypted = downloader.fetchAndDecrypt(
            url = first,
            encrypted = envelope(sources = listOf(first, second), hashValueB64 = sha256B64(ciphertext)),
            declaredSize = plaintext.size.toLong(),
        )

        assertArrayEquals(plaintext, decrypted)
    }

    @Test
    fun `uses the outer url when the envelope lists no sources`() = runTest {
        server.enqueue(ciphertextResponse())

        val decrypted = downloader.fetchAndDecrypt(
            url = server.url("/blob.enc").toString(),
            encrypted = envelope(sources = emptyList()),
            declaredSize = null,
        )

        assertArrayEquals(plaintext, decrypted)
    }

    @Test
    fun `concurrent requests for the same composite key share one download`() = runTest {
        // ONE enqueued response: a second network hit would fail. The
        // body delay keeps the first download in flight until the
        // second caller has joined it.
        server.enqueue(
            MockResponse.Builder()
                .code(200)
                .headersDelay(250, TimeUnit.MILLISECONDS)
                .body(Buffer().write(ciphertext))
                .build(),
        )
        val url = server.url("/blob.enc").toString()
        val envelope = envelope(sources = listOf(url), hashValueB64 = sha256B64(ciphertext))

        val first = async { downloader.fetchAndDecrypt(url, envelope, null) }
        val second = async { downloader.fetchAndDecrypt(url, envelope, null) }

        assertArrayEquals(plaintext, first.await())
        assertArrayEquals(plaintext, second.await())
        assertEquals(1, server.requestCount)
    }

    @Test
    fun `a hash mismatch on every source yields null`() = runTest {
        server.enqueue(ciphertextResponse())
        val url = server.url("/blob.enc").toString()

        assertNull(
            downloader.fetchAndDecrypt(
                url = url,
                encrypted = envelope(sources = listOf(url), hashValueB64 = sha256B64(plaintext)),
                declaredSize = null,
            ),
        )
    }
}
