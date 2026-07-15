package social.waddle.android.client

import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.channels.ReceiveChannel
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleEventListener
import social.waddle.client.ffi.WaddleSmResumeState

/**
 * FFI listener → domain event bridge. Callbacks arrive on Rust/tokio
 * threads via JNA and must never block: every event maps to an
 * [XmppEvent] and is `trySend`-ed into an unbounded channel (the Kotlin
 * analog of Apple's unbounded `AsyncStream`), except resume snapshots
 * which go straight to [onResumeStateChanged] for persistence.
 */
class XmppEventBridge(
    private val onResumeStateChanged: (WaddleSmResumeState?) -> Unit,
) : WaddleEventListener {
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
        when (event) {
            is WaddleClientEvent.Connected -> channel.trySend(XmppEvent.SessionReady)
            is WaddleClientEvent.Disconnected -> channel.trySend(XmppEvent.Disconnected)
            is WaddleClientEvent.Message -> channel.trySend(XmppEvent.Message(event.message))
            is WaddleClientEvent.Presence -> channel.trySend(XmppEvent.Presence(event.presence))
            is WaddleClientEvent.MamResult -> channel.trySend(XmppEvent.MamResult(event.message))
            is WaddleClientEvent.DeliveryAcked -> channel.trySend(XmppEvent.DeliveryAcked(event.stanzaId))
            is WaddleClientEvent.DeliveryFailed -> channel.trySend(XmppEvent.DeliveryFailed(event.stanzaId))
            is WaddleClientEvent.Call -> channel.trySend(XmppEvent.Call(event.event))
            is WaddleClientEvent.ResumeStateChanged -> onResumeStateChanged(event.state)
            is WaddleClientEvent.AuthenticationFailed ->
                channel.trySend(XmppEvent.AuthenticationFailed(event.condition))
            is WaddleClientEvent.Error -> channel.trySend(XmppEvent.Error(event.description))
        }
    }
}
