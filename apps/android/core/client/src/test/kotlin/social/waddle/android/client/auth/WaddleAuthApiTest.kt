package social.waddle.android.client.auth

import java.net.ConnectException
import java.net.SocketException
import java.net.SocketTimeoutException
import java.net.UnknownHostException
import kotlinx.coroutines.test.runTest
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.OkHttpClient
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class WaddleAuthApiTest {
    private lateinit var server: MockWebServer
    private lateinit var api: WaddleAuthApi

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
        api = WaddleAuthApi(server.url("/").toString(), OkHttpClient())
    }

    @After
    fun tearDown() {
        server.close()
    }

    private fun enqueueJson(body: String, code: Int = 200) {
        server.enqueue(MockResponse(code = code, body = body))
    }

    @Test
    fun `providers parses the snake_case fixture`() = runTest {
        enqueueJson("""[{"id":"github","kind":"oauth","display_name":"GitHub"},{"id":"dev","kind":"dev"}]""")

        val providers = api.providers().getOrThrow()

        assertEquals(
            listOf(
                AuthProvider(id = "github", kind = "oauth", displayName = "GitHub"),
                AuthProvider(id = "dev", kind = "dev", displayName = null),
            ),
            providers,
        )
        assertEquals("/api/auth/providers", server.takeRequest().url.encodedPath)
    }

    @Test
    fun `device start parses and defaults the poll interval`() = runTest {
        enqueueJson(
            """
            {"device_code":"dc-1","user_code":"WDL-1234",
             "verification_uri":"https://waddle.test/api/auth/device/verify",
             "verification_uri_complete":"https://waddle.test/api/auth/device/verify?code=WDL-1234",
             "expires_in":600}
            """.trimIndent(),
        )

        val flow = api.startDeviceAuth("github").getOrThrow()

        assertEquals("dc-1", flow.deviceCode)
        assertEquals("WDL-1234", flow.userCode)
        assertEquals(5, flow.interval)
        assertEquals(600, flow.expiresIn)

        val request = server.takeRequest()
        assertEquals("/api/auth/device/start", request.url.encodedPath)
        assertEquals("""{"provider":"github"}""", request.body?.utf8())
    }

    @Test
    fun `poll returns pending until complete carries the session id`() = runTest {
        enqueueJson("""{"status":"pending"}""")
        enqueueJson("""{"status":"complete","session_id":"sess-1"}""")

        assertEquals(DevicePollResult.Pending, api.pollDeviceAuth("dc-1").getOrThrow())
        assertEquals(DevicePollResult.Complete("sess-1"), api.pollDeviceAuth("dc-1").getOrThrow())

        assertEquals("""{"device_code":"dc-1"}""", server.takeRequest().body?.utf8())
    }

    @Test
    fun `poll complete without session id is an invalid response`() = runTest {
        enqueueJson("""{"status":"complete"}""")

        val failure = api.pollDeviceAuth("dc-1").exceptionOrNull()

        assertTrue(failure is AuthError.InvalidResponse)
    }

    @Test
    fun `session parses the full snake_case fixture`() = runTest {
        enqueueJson(
            """
            {"session_id":"sess-1","username":"icepuma","avatar_url":"https://waddle.test/a.png",
             "xmpp_localpart":"icepuma","jid":"icepuma@waddle.test",
             "xmpp_websocket_url":"wss://waddle.test/xmpp",
             "link_preview_media_origin":"https://media.waddle.test",
             "is_expired":false,"expires_at":"2026-08-01T00:00:00Z"}
            """.trimIndent(),
        )

        val session = api.session("sess-1").getOrThrow()

        assertEquals(
            WaddleSessionInfo(
                sessionId = "sess-1",
                username = "icepuma",
                avatarUrl = "https://waddle.test/a.png",
                xmppLocalpart = "icepuma",
                jid = "icepuma@waddle.test",
                xmppWebsocketUrl = "wss://waddle.test/xmpp",
                linkPreviewMediaOrigin = "https://media.waddle.test",
                isExpired = false,
                expiresAt = "2026-08-01T00:00:00Z",
            ),
            session,
        )
        assertEquals("session_id=sess-1", server.takeRequest().url.query)
    }

    @Test
    fun `session resolves null on 401 and 404`() = runTest {
        enqueueJson("", code = 401)
        enqueueJson("", code = 404)

        assertNull(api.session("expired").getOrThrow())
        assertNull(api.session("gone").getOrThrow())
    }

    @Test
    fun `server errors surface the message detail`() = runTest {
        enqueueJson("""{"message":"provider exploded"}""", code = 500)

        val failure = api.providers().exceptionOrNull()

        assertEquals(AuthError.Server(500, "provider exploded"), failure)
    }

    @Test
    fun `logout treats 204 401 and 404 as success`() = runTest {
        enqueueJson("", code = 204)
        assertTrue(api.logout("sess-1").isSuccess)
        assertEquals("""{"session_id":"sess-1"}""", server.takeRequest().body?.utf8())

        enqueueJson("", code = 401)
        assertTrue(api.logout("sess-1").isSuccess)

        enqueueJson("", code = 404)
        assertTrue(api.logout("sess-1").isSuccess)
    }

    @Test
    fun `connection failures classify as network errors`() = runTest {
        server.close()

        val failure = api.providers().exceptionOrNull()

        assertTrue("expected AuthError.Network, got $failure", failure is AuthError.Network)
    }

    @Test
    fun `transient poll classification mirrors the apple helper`() {
        assertTrue(isTransientDeviceAuthPollError(AuthError.Network(SocketTimeoutException("timeout"))))
        assertTrue(isTransientDeviceAuthPollError(AuthError.Network(SocketException("Connection reset"))))
        assertFalse(isTransientDeviceAuthPollError(AuthError.Network(ConnectException("Connection refused"))))
        assertFalse(isTransientDeviceAuthPollError(AuthError.Network(UnknownHostException("waddle.test"))))
        assertFalse(isTransientDeviceAuthPollError(AuthError.Server(500, "boom")))
        assertFalse(isTransientDeviceAuthPollError(IllegalStateException("not an auth error")))
    }
}
