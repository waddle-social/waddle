package social.waddle.android.client.store

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import social.waddle.client.ffi.WaddleRoomMemberEntry

/** Load state of a room's XEP-0045 §9.5 member list. */
enum class MemberListStatus {
    /** A refresh is in flight; [RoomMembersState.members] is the last snapshot. */
    LOADING,

    /** At least one affiliation query succeeded. */
    LOADED,

    /**
     * Every affiliation query failed and nothing was collected (web
     * `RoomMemberListUnavailableError` parity). [RoomMembersState.members]
     * keeps the last synced snapshot so the screen can degrade to
     * "showing last synced members" instead of flashing empty.
     */
    UNAVAILABLE,
}

data class RoomMembersState(
    val status: MemberListStatus = MemberListStatus.LOADING,
    val members: List<WaddleRoomMemberEntry> = emptyList(),
)

/**
 * Per-room member lists fetched via the four per-affiliation
 * `muc#admin` queries. Partial failures are tolerated upstream
 * ([social.waddle.android.client.RoomAdminVerbs.refreshRoomMembers]):
 * a room where e.g. the outcast query is forbidden still shows the
 * tiers that loaded.
 */
class RoomMembersStore {
    private val _rooms = MutableStateFlow<Map<String, RoomMembersState>>(emptyMap())

    /** room bare JID → member list state. */
    val rooms: StateFlow<Map<String, RoomMembersState>> = _rooms.asStateFlow()

    fun markLoading(roomJid: String) {
        _rooms.update { rooms ->
            val previous = rooms[roomJid] ?: RoomMembersState()
            rooms + (roomJid to previous.copy(status = MemberListStatus.LOADING))
        }
    }

    fun applyLoaded(roomJid: String, members: List<WaddleRoomMemberEntry>) {
        _rooms.update { rooms ->
            rooms + (roomJid to RoomMembersState(MemberListStatus.LOADED, members))
        }
    }

    /** All queries failed: degrade but keep the last synced snapshot. */
    fun applyUnavailable(roomJid: String) {
        _rooms.update { rooms ->
            val previous = rooms[roomJid] ?: RoomMembersState()
            rooms + (roomJid to previous.copy(status = MemberListStatus.UNAVAILABLE))
        }
    }

    fun clear() {
        _rooms.value = emptyMap()
    }
}
