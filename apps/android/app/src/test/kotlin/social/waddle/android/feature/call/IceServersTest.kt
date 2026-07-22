package social.waddle.android.feature.call

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.client.ffi.WaddleExternalService
import social.waddle.client.ffi.WaddleExternalServiceTransport
import social.waddle.client.ffi.WaddleExternalServiceType

/** The XEP-0215 → ICE URI mapping rules (web ice-servers.ts parity). */
class IceServersTest {
    private fun service(
        type: WaddleExternalServiceType,
        host: String = "turn.waddle.social",
        port: UShort? = 3478u,
        transport: WaddleExternalServiceTransport? = null,
        username: String? = null,
        password: String? = null,
    ) = WaddleExternalService(
        serviceType = type,
        host = host,
        port = port,
        transport = transport,
        username = username,
        password = password,
        expires = null,
        restricted = false,
    )

    @Test
    fun `stun maps without credentials or transport query`() {
        val configs = iceServerConfigsFrom(
            listOf(
                service(
                    WaddleExternalServiceType.STUN,
                    transport = WaddleExternalServiceTransport.UDP,
                ),
            ),
        )
        assertEquals(listOf(IceServerConfig(url = "stun:turn.waddle.social:3478")), configs)
        assertNull(configs.single().username)
    }

    @Test
    fun `turns maps with credentials and rfc7065 transport`() {
        val configs = iceServerConfigsFrom(
            listOf(
                service(
                    WaddleExternalServiceType.TURNS,
                    port = 443u,
                    transport = WaddleExternalServiceTransport.TCP,
                    username = "u",
                    password = "p",
                ),
            ),
        )
        assertEquals(
            listOf(
                IceServerConfig(
                    url = "turns:turn.waddle.social:443?transport=tcp",
                    username = "u",
                    password = "p",
                ),
            ),
            configs,
        )
    }

    @Test
    fun `turn without credentials is dropped`() {
        assertTrue(
            iceServerConfigsFrom(
                listOf(
                    service(WaddleExternalServiceType.TURN, username = "u", password = null),
                    service(WaddleExternalServiceType.TURN, username = null, password = "p"),
                ),
            ).isEmpty(),
        )
    }

    @Test
    fun `missing port defers to the scheme default`() {
        val configs = iceServerConfigsFrom(
            listOf(service(WaddleExternalServiceType.STUN, port = null)),
        )
        assertEquals("stun:turn.waddle.social", configs.single().url)
    }

    @Test
    fun `ipv6 literal hosts are bracketed`() {
        val configs = iceServerConfigsFrom(
            listOf(service(WaddleExternalServiceType.STUN, host = "2001:db8::1", port = 3478u)),
        )
        assertEquals("stun:[2001:db8::1]:3478", configs.single().url)
    }

    @Test
    fun `call duration formats short and long forms`() {
        assertEquals("0:07", formatCallDuration(7))
        assertEquals("5:30", formatCallDuration(330))
        assertEquals("1:00:01", formatCallDuration(3601))
    }
}
