package social.waddle.android.client.store

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.map
import social.waddle.android.client.bareJid
import social.waddle.client.ffi.WaddlePinAction
import social.waddle.client.ffi.WaddlePinEntry
import social.waddle.client.ffi.WaddlePinEvent

/**
 * `urn:waddle:pin:0` pinned-message ids per room (rooms only — the web
 * client has no DM pinning). Seeded by `fetch_room_pins` on room open,
 * kept live by `<pin-event/>` broadcasts.
 */
class PinStore {
    private val _pinned = MutableStateFlow<Map<String, Set<String>>>(emptyMap())

    /** room bare JID → pinned XEP-0359 stanza ids. */
    val pinned: StateFlow<Map<String, Set<String>>> = _pinned.asStateFlow()

    fun pinnedIds(roomJid: String): Flow<Set<String>> {
        val room = bareJid(roomJid)
        return _pinned.map { it[room].orEmpty() }
    }

    fun seed(roomJid: String, entries: List<WaddlePinEntry>) {
        val room = bareJid(roomJid)
        _pinned.value = _pinned.value + (room to entries.map { it.targetStanzaId }.toSet())
    }

    fun onPinEvent(roomJid: String, event: WaddlePinEvent) {
        val room = bareJid(roomJid)
        val current = _pinned.value[room].orEmpty()
        val next = when (event.action) {
            WaddlePinAction.PINNED -> current + event.targetStanzaId
            WaddlePinAction.UNPINNED -> current - event.targetStanzaId
        }
        _pinned.value = _pinned.value + (room to next)
    }

    fun clear() {
        _pinned.value = emptyMap()
    }
}
