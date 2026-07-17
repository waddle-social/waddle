package social.waddle.android.client

import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.channels.ReceiveChannel
import social.waddle.android.client.prefs.toDomain
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleSessionReadyKind

/**
 * Node-local events produced by Kotlin joins the same serialized consumer as
 * pulled native events. Native delivery identity is copied from the typed FFI
 * signal; the bridge never guesses from mutable connection state.
 */
class XmppEventBridge {
    private val channel = Channel<XmppEvent>(Channel.UNLIMITED)

    val events: ReceiveChannel<XmppEvent> = channel

    fun submit(event: XmppEvent) {
        channel.trySend(event)
    }
}

/**
 * Convert one non-control native event. Resume transition and resume snapshot
 * events are durability barriers handled directly by ConnectionLoop.
 */
internal fun WaddleClientEvent.toXmppEvent(ownerBareJid: String): XmppEvent? =
    when (this) {
        is WaddleClientEvent.SessionReady -> XmppEvent.SessionReady(
            kind = when (kind) {
                WaddleSessionReadyKind.FRESH -> SessionReadyKind.FRESH
                WaddleSessionReadyKind.RESUMED -> SessionReadyKind.RESUMED
            },
            attempt = attempt.toDomain(ownerBareJid),
        )
        WaddleClientEvent.Disconnected -> XmppEvent.Disconnected
        is WaddleClientEvent.Message -> XmppEvent.Message(message)
        is WaddleClientEvent.Presence -> XmppEvent.Presence(presence)
        is WaddleClientEvent.MamResult -> XmppEvent.MamResult(message)
        is WaddleClientEvent.DeliveryAcked -> XmppEvent.NativeDeliveryAcked(
            attempt = signal.attempt.toDomain(ownerBareJid),
            clientStanzaId = signal.stanzaId.value,
        )
        is WaddleClientEvent.DeliveryFailed -> XmppEvent.NativeDeliveryFailed(
            attempt = signal.attempt.toDomain(ownerBareJid),
            clientStanzaId = signal.stanzaId.value,
        )
        is WaddleClientEvent.Call -> XmppEvent.Call(event)
        is WaddleClientEvent.AuthenticationFailed -> XmppEvent.AuthenticationFailed(condition)
        is WaddleClientEvent.Error -> XmppEvent.Error(description)
        is WaddleClientEvent.ResumeFailed,
        is WaddleClientEvent.ResumeStateChanged,
        -> null
    }
