package social.waddle.android.feature.call

import social.waddle.client.ffi.WaddleExternalService
import social.waddle.client.ffi.WaddleExternalServiceTransport
import social.waddle.client.ffi.WaddleExternalServiceType

/**
 * SDK-free ICE server description mapped from XEP-0215 external
 * services; the LiveKit controller converts these to
 * `PeerConnection.IceServer` at connect time. Kept free of WebRTC
 * types so the mapping rules stay JVM-unit-testable.
 */
data class IceServerConfig(
    val url: String,
    val username: String? = null,
    val password: String? = null,
)

/**
 * Map XEP-0215 services to ICE server URIs — the exact rules of the
 * web client's ice-servers.ts:
 *
 * - `stun`  → `stun:host[:port]` (no credentials, no `?transport` — RFC 7064)
 * - `turn`  → `turn:host[:port][?transport=…]` with username + credential
 * - `turns` → `turns:host[:port][?transport=…]` (TLS-protected TURN)
 *
 * A TURN/TURNS entry without BOTH username and password is dropped: a
 * relay the client cannot authenticate to is unusable and only adds
 * ICE gathering latency. The `?transport` parameter is TURN-specific
 * (RFC 7065). IPv6 literal hosts are bracketed (RFC 3986/7064) so the
 * port colon stays unambiguous; a missing port defers to the scheme
 * default.
 */
fun iceServerConfigsFrom(services: List<WaddleExternalService>): List<IceServerConfig> =
    services.mapNotNull { service ->
        val hostPort = formatHostPort(service.host, service.port)
        when (service.serviceType) {
            WaddleExternalServiceType.STUN -> IceServerConfig(url = "stun:$hostPort")
            WaddleExternalServiceType.TURN -> turnConfig("turn", hostPort, service)
            WaddleExternalServiceType.TURNS -> turnConfig("turns", hostPort, service)
        }
    }

private fun turnConfig(
    scheme: String,
    hostPort: String,
    service: WaddleExternalService,
): IceServerConfig? {
    val username = service.username ?: return null
    val password = service.password ?: return null
    val transportQuery = when (service.transport) {
        WaddleExternalServiceTransport.UDP -> "?transport=udp"
        WaddleExternalServiceTransport.TCP -> "?transport=tcp"
        null -> ""
    }
    return IceServerConfig(
        url = "$scheme:$hostPort$transportQuery",
        username = username,
        password = password,
    )
}

private fun formatHostPort(host: String, port: UShort?): String {
    val bracketed = if (':' in host) "[$host]" else host
    return if (port == null) bracketed else "$bracketed:$port"
}
