package social.waddle.android.client.store

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import social.waddle.client.ffi.WaddleChannel
import social.waddle.client.ffi.WaddleTopology

/** Server topology (spaces + channels) and the set of joined room JIDs. */
class RoomStore {
    private val _topology = MutableStateFlow(EMPTY_TOPOLOGY)
    val topology: StateFlow<WaddleTopology> = _topology.asStateFlow()

    private val _channels = MutableStateFlow<List<WaddleChannel>>(emptyList())

    /**
     * The listable channels: every topology channel that is NOT a
     * `urn:waddle:group-dm:0` room. Group DMs stay joined for live
     * traffic but belong on the DM surface, never the channel list.
     */
    val channels: StateFlow<List<WaddleChannel>> = _channels.asStateFlow()

    private val _groupDms = MutableStateFlow<List<WaddleChannel>>(emptyList())

    /**
     * The `urn:waddle:group-dm:0` rooms of the topology — the DM-list
     * surface (stage 3) selects from here.
     */
    val groupDms: StateFlow<List<WaddleChannel>> = _groupDms.asStateFlow()

    private val _joinedRooms = MutableStateFlow<Set<String>>(emptySet())
    val joinedRooms: StateFlow<Set<String>> = _joinedRooms.asStateFlow()

    fun setTopology(topology: WaddleTopology) {
        _topology.value = topology
        _channels.value = topology.channels.filterNot { it.isGroupDm }
        _groupDms.value = topology.channels.filter { it.isGroupDm }
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
        _channels.value = emptyList()
        _groupDms.value = emptyList()
        _joinedRooms.value = emptySet()
    }

    private companion object {
        val EMPTY_TOPOLOGY = WaddleTopology(spaces = emptyList(), channels = emptyList())
    }
}
