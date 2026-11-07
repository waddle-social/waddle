package social.waddle.android.client

import kotlinx.coroutines.test.runTest
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Request
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference

class GifSearchTest {
    private val server = MockWebServer()

    @Before
    fun startServer() {
        server.start()
    }

    @After
    fun closeServer() {
        server.close()
    }

    private val payload = """
        {"data":[
          {"id":"g1","title":"Penguin","images":{
            "fixed_height_small":{"url":"https://media0.giphy.com/g1/200s.gif"},
            "original":{"url":"https://media0.giphy.com/g1/giphy.gif"}}},
          {"id":"g2","images":{
            "fixed_height_small":{"url":"https://media0.giphy.com/g2/200s.gif"},
            "original":{"url":"https://media0.giphy.com/g2/giphy.gif"}}},
          {"id":"broken","images":{}}
        ]}
    """.trimIndent()

    @Test
    fun `success payload parses entries and drops broken ones`() {
        val result = classifyGifSearchResponse(200, payload)
        val results = result as GifSearchResult.Results
        assertEquals(listOf("g1", "g2"), results.gifs.map { it.id })
        assertEquals("Penguin", results.gifs[0].title)
        assertEquals("", results.gifs[1].title)
        assertEquals("https://media0.giphy.com/g1/200s.gif", results.gifs[0].previewUrl)
        assertEquals("https://media0.giphy.com/g1/giphy.gif", results.gifs[0].originalUrl)
    }

    @Test
    fun `status codes map to the web state machine`() {
        assertEquals(GifSearchResult.NotConfigured, classifyGifSearchResponse(503, null))
        assertEquals(GifSearchResult.NotConfigured, classifyGifSearchResponse(404, null))
        assertEquals(GifSearchResult.RateLimited, classifyGifSearchResponse(429, null))
        assertEquals(GifSearchResult.Unavailable, classifyGifSearchResponse(502, null))
        assertEquals(GifSearchResult.Unavailable, classifyGifSearchResponse(500, null))
    }

    @Test
    fun `malformed success bodies are unavailable`() {
        assertEquals(GifSearchResult.Unavailable, classifyGifSearchResponse(200, "not json"))
        assertEquals(GifSearchResult.Unavailable, classifyGifSearchResponse(200, "{}"))
        assertEquals(GifSearchResult.Unavailable, classifyGifSearchResponse(200, null))
    }

    @Test
    fun `empty data array is a valid empty result`() {
        val result = classifyGifSearchResponse(200, """{"data":[]}""")
        assertTrue((result as GifSearchResult.Results).gifs.isEmpty())
    }

    @Test
    fun `production auth uses chat proxy for trending without credentials`() = runTest {
        val captured = AtomicReference<Request>()
        val client = OkHttpClient.Builder().addInterceptor { chain ->
            val request = chain.request()
            captured.set(request)
            Response.Builder()
                .request(request)
                .protocol(Protocol.HTTP_1_1)
                .code(200)
                .message("OK")
                .body(payload.toResponseBody())
                .build()
        }.build()

        val result = GiphyProxyGateway("https://xmpp.waddle.social", client).search("")

        assertEquals(2, (result as GifSearchResult.Results).gifs.size)
        assertEquals("https://waddle.chat/api/giphy?limit=24", captured.get().url.toString())
        assertNull(captured.get().header("Authorization"))
        assertNull(captured.get().header("Cookie"))
    }

    @Test
    fun `custom server retains its own base path`() = runTest {
        val captured = AtomicReference<Request>()
        val client = OkHttpClient.Builder().addInterceptor { chain ->
            val request = chain.request()
            captured.set(request)
            Response.Builder()
                .request(request)
                .protocol(Protocol.HTTP_1_1)
                .code(200)
                .message("OK")
                .body(payload.toResponseBody())
                .build()
        }.build()

        val result = GiphyProxyGateway("https://community.example/app/", client).search("cats")

        assertTrue(result is GifSearchResult.Results)
        assertEquals("https://community.example/app/api/giphy?limit=24&q=cats", captured.get().url.toString())
    }

    @Test
    fun `explicit proxy receives encoded search and returns real HTTP response`() = runTest {
        server.enqueue(MockResponse(code = 200, body = payload))
        val gateway = GiphyProxyGateway(
            serverUrl = "https://xmpp.waddle.social",
            httpClient = OkHttpClient(),
            proxyUrl = server.url("/gif-proxy/").toString(),
        )

        val result = gateway.search("  cats & penguins 🐧  ")
        val request = server.takeRequest()

        assertEquals(2, (result as GifSearchResult.Results).gifs.size)
        assertEquals("GET", request.method)
        assertEquals("/gif-proxy/api/giphy", request.url.encodedPath)
        assertEquals("24", request.url.queryParameter("limit"))
        assertEquals("cats & penguins 🐧", request.url.queryParameter("q"))
        assertNull(request.headers["Authorization"])
        assertNull(request.headers["Cookie"])
    }

    @Test
    fun `proxy HTTP errors retain their picker states`() = runTest {
        val gateway = GiphyProxyGateway(server.url("/").toString(), OkHttpClient())
        for ((status, expected) in listOf(
            404 to GifSearchResult.NotConfigured,
            503 to GifSearchResult.NotConfigured,
            429 to GifSearchResult.RateLimited,
            502 to GifSearchResult.Unavailable,
        )) {
            server.enqueue(MockResponse(code = status))
            assertEquals(expected, gateway.search("test"))
        }
    }

    @Test
    fun `invalid proxy configuration returns not configured without a request`() = runTest {
        val calls = AtomicInteger()
        val client = OkHttpClient.Builder().addInterceptor { _ ->
            calls.incrementAndGet()
            error("Invalid configuration must not send a request")
        }.build()
        for (value in listOf(
            "https://user:secret@proxy.example",
            "https://proxy.example/?token=secret",
            "https://proxy.example/#fragment",
            "https://proxy.example:65536",
            "file:///tmp/gif",
            "not-a-url",
        )) {
            val gateway = GiphyProxyGateway("https://xmpp.waddle.social", client, value)
            assertEquals(value, GifSearchResult.NotConfigured, gateway.search("private query"))
        }
        val unsafeServer = GiphyProxyGateway("https://user:secret@community.example", client)
        assertEquals(GifSearchResult.NotConfigured, unsafeServer.search("private query"))
        assertEquals(0, calls.get())
    }
}
