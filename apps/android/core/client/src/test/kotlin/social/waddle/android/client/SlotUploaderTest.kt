package social.waddle.android.client

import kotlinx.coroutines.test.runTest
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.OkHttpClient
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import social.waddle.client.ffi.WaddleUploadHeader
import social.waddle.client.ffi.WaddleUploadSlot
import java.io.ByteArrayInputStream

class SlotUploaderTest {
    private lateinit var server: MockWebServer
    private lateinit var uploader: SlotUploader

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
        uploader = SlotUploader(OkHttpClient())
    }

    @After
    fun tearDown() {
        server.close()
    }

    private fun slot(headers: List<WaddleUploadHeader> = emptyList()) = WaddleUploadSlot(
        putUrl = server.url("/upload/abc/cat.png").toString(),
        getUrl = "https://files.waddle.test/abc/cat.png",
        putHeaders = headers,
    )

    @Test
    fun `puts the bytes with content type and slot headers`() = runTest {
        server.enqueue(MockResponse(code = 201))
        val payload = "png-bytes".toByteArray()

        val ok = uploader.put(
            slot = slot(headers = listOf(WaddleUploadHeader(name = "Authorization", value = "token"))),
            contentType = "image/png",
            contentLength = payload.size.toLong(),
            open = { ByteArrayInputStream(payload) },
        )

        assertTrue(ok)
        val recorded = server.takeRequest()
        assertEquals("PUT", recorded.method)
        assertEquals("image/png", recorded.headers["Content-Type"])
        assertEquals("token", recorded.headers["Authorization"])
        assertEquals("png-bytes", recorded.body?.utf8())
    }

    @Test
    fun `a rejecting service fails the upload`() = runTest {
        server.enqueue(MockResponse(code = 403))

        val ok = uploader.put(
            slot = slot(),
            contentType = "image/png",
            contentLength = 3,
            open = { ByteArrayInputStream(byteArrayOf(1, 2, 3)) },
        )

        assertFalse(ok)
    }

    @Test
    fun `an unopenable source fails instead of throwing`() = runTest {
        server.enqueue(MockResponse(code = 201))

        val ok = uploader.put(
            slot = slot(),
            contentType = "image/png",
            contentLength = 3,
            open = { null },
        )

        assertFalse(ok)
    }
}
