package social.waddle.android.client.server

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class ServerOriginTest {
    private fun ok(input: String) = ServerOrigin.normalize(input).getOrThrow()

    private fun errorOf(input: String) =
        (ServerOrigin.normalize(input).exceptionOrNull() as ServerOriginException).error

    @Test
    fun `bare host gets https`() = assertEquals("https://chat.example", ok("chat.example"))

    @Test
    fun `whitespace and trailing slash are dropped`() =
        assertEquals("https://chat.example", ok("  https://chat.example/  "))

    @Test
    fun `path query fragment and credentials are stripped`() =
        assertEquals("https://chat.example", ok("https://u:p@chat.example/a/b?x=1#f"))

    @Test
    fun `non default port is kept and default elided`() {
        assertEquals("https://chat.example:8443", ok("chat.example:8443"))
        assertEquals("https://chat.example", ok("https://chat.example:443"))
    }

    @Test
    fun `host case is normalized`() = assertEquals("https://chat.example", ok("CHAT.Example"))

    @Test
    fun `blank is rejected`() = assertEquals(ServerOriginError.EMPTY, errorOf("   "))

    @Test
    fun `cleartext is rejected off loopback`() {
        assertEquals(ServerOriginError.INSECURE_SCHEME, errorOf("http://chat.example"))
        assertEquals(ServerOriginError.INSECURE_SCHEME, errorOf("http://10.0.2.2:5280"))
    }

    @Test
    fun `cleartext is allowed on loopback`() {
        assertEquals("http://localhost:5280", ok("http://localhost:5280"))
        assertEquals("http://127.0.0.1:5280", ok("http://127.0.0.1:5280"))
    }

    @Test
    fun `other schemes and junk are invalid`() {
        assertEquals(ServerOriginError.INVALID, errorOf("ftp://chat.example"))
        assertEquals(ServerOriginError.INVALID, errorOf("chat example"))
    }

    @Test
    fun `display shows host and non default port`() {
        assertEquals("chat.example", ServerOrigin.display("https://chat.example"))
        assertEquals("chat.example:8443", ServerOrigin.display("https://chat.example:8443"))
        assertTrue(ServerOrigin.display("not a url") == "not a url")
    }
}
