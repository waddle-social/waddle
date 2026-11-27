package social.waddle.android.client.server

import kotlinx.coroutines.test.runTest
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.OkHttpClient
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test

class ServerDiscoveryTest {
    private lateinit var server: MockWebServer
    private val discovery = ServerDiscovery(OkHttpClient())

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
    }

    @After
    fun tearDown() {
        server.close()
    }

    private fun address() = "http://localhost:${server.port}"

    private fun errorOf(result: Result<String>) =
        (result.exceptionOrNull() as ServerDiscoveryException).error

    @Test
    fun `an origin answering the providers list is accepted as typed`() = runTest {
        server.enqueue(MockResponse(code = 200, body = """[{"id":"dev","kind":"dev"}]"""))

        val result = discovery.discover(address() + "/some/path")

        assertEquals(address(), result.getOrThrow())
        assertEquals("/api/auth/providers", server.takeRequest().url.encodedPath)
    }

    @Test
    fun `a server that is not Waddle is rejected`() = runTest {
        server.enqueue(MockResponse(code = 404, body = "nope"))
        server.enqueue(MockResponse(code = 404, body = "nope"))

        assertEquals(ServerDiscoveryError.NOT_A_WADDLE_SERVER, errorOf(discovery.discover(address())))
    }

    @Test
    fun `a 200 that is not a provider list is rejected`() = runTest {
        server.enqueue(MockResponse(code = 200, body = "<html>hello</html>"))
        server.enqueue(MockResponse(code = 404, body = ""))

        assertEquals(ServerDiscoveryError.NOT_A_WADDLE_SERVER, errorOf(discovery.discover(address())))
    }

    @Test
    fun `nothing listening is unreachable`() = runTest {
        val dead = address()
        server.close()

        assertEquals(ServerDiscoveryError.UNREACHABLE, errorOf(discovery.discover(dead)))
    }

    @Test
    fun `cleartext off loopback is refused before any request`() = runTest {
        assertEquals(
            ServerDiscoveryError.INSECURE_SCHEME,
            errorOf(discovery.discover("http://chat.example")),
        )
        assertEquals(0, server.requestCount)
    }

    @Test
    fun `host-meta websocket link names the service origin`() {
        val json = """{"links":[{"rel":"urn:xmpp:alt-connections:xbosh","href":"https://x/b"},""" +
            """{"rel":"urn:xmpp:alt-connections:websocket","href":"wss://xmpp.chat.example/ws"}]}"""
        assertEquals("https://xmpp.chat.example", ServerDiscovery.websocketOrigin(json))
    }

    @Test
    fun `host-meta keeps a non default port`() {
        val json = """{"links":[{"rel":"urn:xmpp:alt-connections:websocket","href":"wss://h.example:8443/ws"}]}"""
        assertEquals("https://h.example:8443", ServerDiscovery.websocketOrigin(json))
    }

    @Test
    fun `host-meta without a secure websocket link yields nothing`() {
        assertNull(ServerDiscovery.websocketOrigin("""{"links":[]}"""))
        assertNull(
            ServerDiscovery.websocketOrigin(
                """{"links":[{"rel":"urn:xmpp:alt-connections:websocket","href":"ws://h.example/ws"}]}""",
            ),
        )
        assertNull(ServerDiscovery.websocketOrigin("not json"))
    }
}
