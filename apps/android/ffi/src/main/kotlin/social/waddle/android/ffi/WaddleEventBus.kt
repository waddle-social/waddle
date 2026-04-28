package social.waddle.android.ffi

import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.asSharedFlow
import uniffi.waddle_xmpp_client.WaddleArchivedMessage
import uniffi.waddle_xmpp_client.WaddleEventListener
import uniffi.waddle_xmpp_client.WaddleMessage
import uniffi.waddle_xmpp_client.WaddlePresence

/**
 * Bridges the uniffi-generated [WaddleEventListener] callback interface
 * onto a coroutine [SharedFlow]. The listener methods are invoked on
 * uniffi's internal thread pool, so this class does the bare minimum —
 * wrap the typed event and `tryEmit` — and never blocks on the consumer.
 *
 * Drop-oldest backpressure prevents a slow UI consumer from stalling the
 * Rust client's callback path.
 */
public class WaddleEventBus : WaddleEventListener {
    private val mutable = MutableSharedFlow<WaddleEvent>(
        replay = 0,
        extraBufferCapacity = EVENT_BUFFER,
        onBufferOverflow = BufferOverflow.DROP_OLDEST,
    )

    public val events: SharedFlow<WaddleEvent> = mutable.asSharedFlow()

    override fun onMessage(message: WaddleMessage) {
        mutable.tryEmit(WaddleEvent.Message(message))
    }

    override fun onPresence(presence: WaddlePresence) {
        mutable.tryEmit(WaddleEvent.Presence(presence))
    }

    override fun onMamResult(message: WaddleArchivedMessage) {
        mutable.tryEmit(WaddleEvent.MamResult(message))
    }

    override fun onConnected() {
        mutable.tryEmit(WaddleEvent.Connected)
    }

    override fun onDisconnected() {
        mutable.tryEmit(WaddleEvent.Disconnected)
    }

    override fun onError(description: String) {
        mutable.tryEmit(WaddleEvent.Error(description))
    }

    private companion object {
        const val EVENT_BUFFER = 64
    }
}
