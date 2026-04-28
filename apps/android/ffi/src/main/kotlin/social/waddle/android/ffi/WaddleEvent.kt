package social.waddle.android.ffi

import uniffi.waddle_xmpp_client.WaddleArchivedMessage
import uniffi.waddle_xmpp_client.WaddleMessage
import uniffi.waddle_xmpp_client.WaddlePresence

/**
 * Typed wrapper over the events that arrive on the
 * [uniffi.waddle_xmpp_client.WaddleEventListener] callback interface.
 *
 * Mirrors the six callback methods exactly. No `String`-typed payloads on
 * the wire side; the only string is [Error.description], a human-facing
 * log string emitted by the Rust client.
 */
public sealed interface WaddleEvent {
    public data class Message(val message: WaddleMessage) : WaddleEvent

    public data class Presence(val presence: WaddlePresence) : WaddleEvent

    public data class MamResult(val message: WaddleArchivedMessage) : WaddleEvent

    public data object Connected : WaddleEvent

    public data object Disconnected : WaddleEvent

    public data class Error(val description: String) : WaddleEvent
}
