package social.waddle.android.client.store

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update
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

    /** Per-room live-event counter; snapshot seeds check staleness. */
    private val eventVersions = java.util.concurrent.ConcurrentHashMap<String, Long>()

    fun pinnedIds(roomJid: String): Flow<Set<String>> {
        val room = bareJid(roomJid)
        return _pinned.map { it[room].orEmpty() }
    }

    /** The room's current live-event version (capture before a fetch). */
    fun eventVersion(roomJid: String): Long = eventVersions[bareJid(roomJid)] ?: 0L

    /**
     * Replace the room's pin set with a fetched snapshot — but only
     * when no live pin event landed since [fetchedAtVersion] was
     * captured: a snapshot raced by a broadcast is stale and would
     * clobber the newer wire state (the next room open re-fetches).
     */
    fun seed(roomJid: String, entries: List<WaddlePinEntry>, fetchedAtVersion: Long) {
        val room = bareJid(roomJid)
        if ((eventVersions[room] ?: 0L) != fetchedAtVersion) return
        _pinned.update { it + (room to entries.map { entry -> entry.targetStanzaId }.toSet()) }
    }

    fun onPinEvent(roomJid: String, event: WaddlePinEvent) {
        val room = bareJid(roomJid)
        eventVersions.merge(room, 1L, Long::plus)
        // CAS: pin fetches and live pin events race across coroutines.
        _pinned.update { pinned ->
            val next = when (event.action) {
                WaddlePinAction.PINNED -> pinned[room].orEmpty() + event.targetStanzaId
                WaddlePinAction.UNPINNED -> pinned[room].orEmpty() - event.targetStanzaId
            }
            pinned + (room to next)
        }
    }

    fun clear() {
        _pinned.value = emptyMap()
        eventVersions.clear()
    }
}
