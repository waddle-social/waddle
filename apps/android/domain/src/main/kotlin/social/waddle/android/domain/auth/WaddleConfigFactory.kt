package social.waddle.android.domain.auth

import uniffi.waddle_xmpp_client.WaddleConfig

/**
 * Mirrors `apps/apple/Waddle/XMPP/XMPPTypes.swift`'s
 * `XMPPCredentials.init(session:)`: derive a deterministic resource
 * suffix from the session id so the FFI client doesn't end up reusing
 * the same JID/resource pair across distinct sessions.
 */
public fun AuthSession.toWaddleConfig(): WaddleConfig {
    val suffix = sessionId.take(8).ifEmpty { "client" }
    return WaddleConfig(
        serverUrl = xmppWebsocketUrl,
        jid = jid,
        accessToken = sessionId,
        resource = "waddle-$suffix",
    )
}
