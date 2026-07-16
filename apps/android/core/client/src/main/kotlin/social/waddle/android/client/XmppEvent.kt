package social.waddle.android.client

import social.waddle.client.ffi.WaddleArchivedMessage
import social.waddle.client.ffi.WaddleCallEvent
import social.waddle.client.ffi.WaddleMdsDisplayedEntry
import social.waddle.client.ffi.WaddleMessage
import social.waddle.client.ffi.WaddlePinEntry
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

    /**
     * The message with this id will not be delivered: a XEP-0198
     * transport-level failure, or a dropped outbound-queue entry (cap
     * eviction / permanent replay rejection).
     */
    data class DeliveryFailed(val stanzaId: String) : XmppEvent

    data class Call(val event: WaddleCallEvent) : XmppEvent

    /**
     * XEP-0490 catch-up entries from the connect bootstrap, injected
     * into the event stream so cursor/badge recomputes serialize with
     * live-message unread increments. [applied] (when present) is
     * completed by the consumer after application — the ready pipeline
     * joins it so later steps (the pending-displayed drain) order after
     * the fetched cursors.
     */
    data class MdsEntries(
        val entries: List<WaddleMdsDisplayedEntry>,
        val applied: kotlinx.coroutines.CompletableJob? = null,
    ) : XmppEvent

    /**
     * `urn:waddle:pin:0` room snapshot from `fetch_room_pins`, injected
     * into the event stream so the seed serializes with live pin-event
     * broadcasts instead of clobbering them.
     */
    data class RoomPins(
        val roomJid: String,
        val entries: List<WaddlePinEntry>,
        /** PinStore event version captured before the fetch started. */
        val fetchedAtVersion: Long,
    ) : XmppEvent

    /**
     * A sibling device's XEP-0490 cursor covered everything unread in
     * this conversation — shade notifications for it are stale.
     */
    data class ReadSynced(val conversationJid: String) : XmppEvent

    /**
     * RFC 6120 §6.5 SASL failure: terminal for the presented token —
     * the session manager signs out instead of re-presenting a dead
     * credential forever (web #1164 parity).
     */
    data class AuthenticationFailed(val condition: WaddleSaslCondition) : XmppEvent

    /** Human-readable diagnostic; never carries protocol data. */
    data class Error(val description: String) : XmppEvent
}
