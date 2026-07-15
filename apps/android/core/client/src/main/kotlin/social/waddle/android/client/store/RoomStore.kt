package social.waddle.android.client.store

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import social.waddle.client.ffi.WaddleTopology

/** Server topology (spaces + channels) and the set of joined room JIDs. */
class RoomStore {
    private val _topology = MutableStateFlow(EMPTY_TOPOLOGY)
    val topology: StateFlow<WaddleTopology> = _topology.asStateFlow()

    private val _joinedRooms = MutableStateFlow<Set<String>>(emptySet())
    val joinedRooms: StateFlow<Set<String>> = _joinedRooms.asStateFlow()

    fun setTopology(topology: WaddleTopology) {
        _topology.value = topology
    }

    fun replaceJoinedRooms(rooms: Set<String>) {
        _joinedRooms.value = rooms
    }

    fun markJoined(roomJid: String) {
        _joinedRooms.update { it + roomJid }
    }

    fun markLeft(roomJid: String) {
        _joinedRooms.update { it - roomJid }
    }

    fun clear() {
        _topology.value = EMPTY_TOPOLOGY
        _joinedRooms.value = emptySet()
    }

    private companion object {
        val EMPTY_TOPOLOGY = WaddleTopology(spaces = emptyList(), channels = emptyList())
    }
}
