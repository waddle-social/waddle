package social.waddle.android.domain

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.filterIsInstance
import kotlinx.coroutines.flow.launchIn
import kotlinx.coroutines.flow.onEach
import social.waddle.android.ffi.WaddleClientHandle
import social.waddle.android.ffi.WaddleEvent
import uniffi.waddle_xmpp_client.WaddlePresence

public class PresenceRepository(
    private val client: WaddleClientHandle,
    scope: CoroutineScope,
) {
    private val mutable = MutableStateFlow<Map<String, WaddlePresence>>(emptyMap())
    public val byJid: StateFlow<Map<String, WaddlePresence>> = mutable.asStateFlow()

    init {
        client.events
            .filterIsInstance<WaddleEvent.Presence>()
            .onEach { event ->
                val from = event.presence.from ?: return@onEach
                mutable.value = mutable.value + (from.substringBefore('/') to event.presence)
            }
            .launchIn(scope)
    }

    public suspend fun publish(status: String? = null, show: String? = null): Unit =
        client.sendPresence(status, show)
}
