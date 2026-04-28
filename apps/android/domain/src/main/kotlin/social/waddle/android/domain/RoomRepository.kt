package social.waddle.android.domain

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.filterIsInstance
import kotlinx.coroutines.flow.onEach
import kotlinx.coroutines.flow.launchIn
import social.waddle.android.ffi.WaddleClientHandle
import social.waddle.android.ffi.WaddleEvent
import uniffi.waddle_xmpp_client.WaddleRoom

/**
 * Caches the room list returned by `list_rooms()` and refreshes it when
 * the client reconnects. Rooms are typed `WaddleRoom` straight from the
 * FFI — no string-based shadow models in this layer.
 */
public class RoomRepository(
    private val client: WaddleClientHandle,
    scope: CoroutineScope,
) {
    private val mutable = MutableStateFlow<List<WaddleRoom>>(emptyList())
    public val rooms: StateFlow<List<WaddleRoom>> = mutable.asStateFlow()

    init {
        client.events
            .filterIsInstance<WaddleEvent.Connected>()
            .onEach { refresh() }
            .launchIn(scope)
    }

    public suspend fun refresh() {
        mutable.value = client.listRooms().sortedBy { it.position }
    }

    public suspend fun join(roomJid: String, nick: String): Unit =
        client.joinRoom(roomJid, nick)

    public suspend fun leave(roomJid: String, nick: String): Unit =
        client.leaveRoom(roomJid, nick)
}
