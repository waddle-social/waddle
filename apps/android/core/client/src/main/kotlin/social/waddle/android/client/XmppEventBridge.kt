package social.waddle.android.client

import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.channels.ReceiveChannel
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleEventListener

/**
 * FFI listener → domain event bridge. Callbacks arrive on Rust/tokio
 * threads via JNA and must never block: every event maps to an
 * [XmppEvent] and is `trySend`-ed into an unbounded channel (the Kotlin
 * analog of Apple's unbounded `AsyncStream`).
 */
class XmppEventBridge : WaddleEventListener {
    private val channel = Channel<XmppEvent>(Channel.UNLIMITED)

    /** Single-consumer stream drained by `XmppSessionManager`. */
    val events: ReceiveChannel<XmppEvent> = channel

    /**
     * Inject a locally-produced event into the SAME serialized stream
     * the wire events flow through — consumers that read-modify-write
     * shared state (unread recomputes vs live increments) rely on this
     * single-consumer ordering.
     */
    fun submit(event: XmppEvent) {
        channel.trySend(event)
    }

    override fun onEvent(event: WaddleClientEvent) {
        val translated = sessionEventOf(event) ?: stanzaEventOf(event) ?: return
        channel.trySend(translated)
    }

    /** Stream lifecycle and diagnostics; `null` for stanza-borne events. */
    private fun sessionEventOf(event: WaddleClientEvent): XmppEvent? = when (event) {
        is WaddleClientEvent.Connected -> XmppEvent.SessionReady
        is WaddleClientEvent.Disconnected -> XmppEvent.Disconnected
        is WaddleClientEvent.AuthenticationFailed -> XmppEvent.AuthenticationFailed(event.condition)
        is WaddleClientEvent.Error -> XmppEvent.Error(event.description)
        else -> null
    }

    /** Events carried by stanzas; `null` for those with no domain event. */
    private fun stanzaEventOf(event: WaddleClientEvent): XmppEvent? = when (event) {
        is WaddleClientEvent.Message -> XmppEvent.Message(event.message)
        is WaddleClientEvent.Presence -> XmppEvent.Presence(event.presence)
        is WaddleClientEvent.DeliveryAcked -> XmppEvent.DeliveryAcked(event.stanzaId)
        is WaddleClientEvent.DeliveryFailed -> XmppEvent.DeliveryFailed(event.stanzaId)
        is WaddleClientEvent.MessageRejected -> XmppEvent.MessageRejected(event.stanzaId, event.from, event.to)
        is WaddleClientEvent.AvatarChanged -> XmppEvent.AvatarChanged(event.jid, event.avatarId)
        is WaddleClientEvent.StatusPreferenceChanged -> XmppEvent.StatusPreferenceChanged(event.preference)
        is WaddleClientEvent.Call -> XmppEvent.Call(event.event)
        is WaddleClientEvent.InboxPush -> XmppEvent.InboxPush(event.entry)
        // MamResult: history arrives as the page a fetch returns; the
        // per-result echo would ingest every archived row a second time.
        else -> null
    }
}
