package social.waddle.android.client

import social.waddle.client.ffi.WaddleArchivedMessage
import social.waddle.client.ffi.WaddleCallEvent
import social.waddle.client.ffi.WaddleMessage
import social.waddle.client.ffi.WaddlePresence
import social.waddle.client.ffi.WaddleSaslCondition

/**
 * Kotlin domain event stream the app consumes — a thin mirror of the FFI
 * `WaddleClientEvent` reusing the FFI records as payloads. Resume-state
 * changes are not part of this stream; they flow through the
 * [XmppEventBridge] persistence callback instead.
 */
sealed interface XmppEvent {
    /** Session is bound and ready (FFI `Connected`). */
    data object SessionReady : XmppEvent

    /** The event stream closed; no further events will fire. */
    data object Disconnected : XmppEvent

    data class Message(val message: WaddleMessage) : XmppEvent

    data class Presence(val presence: WaddlePresence) : XmppEvent

    data class MamResult(val message: WaddleArchivedMessage) : XmppEvent

    /** XEP-0198: the server acked the outbound message with this id. */
    data class DeliveryAcked(val stanzaId: String) : XmppEvent

    /** XEP-0198: transport-level delivery failure for this id. */
    data class DeliveryFailed(val stanzaId: String) : XmppEvent

    data class Call(val event: WaddleCallEvent) : XmppEvent

    /**
     * RFC 6120 §6.5 SASL failure: terminal for the presented token —
     * the session manager signs out instead of re-presenting a dead
     * credential forever (web #1164 parity).
     */
    data class AuthenticationFailed(val condition: WaddleSaslCondition) : XmppEvent

    /** Human-readable diagnostic; never carries protocol data. */
    data class Error(val description: String) : XmppEvent
}
