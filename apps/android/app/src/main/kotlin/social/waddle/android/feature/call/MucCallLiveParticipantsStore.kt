package social.waddle.android.feature.call

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * App-scoped projection of the LiveKit SFU's participant truth for the
 * MUC room our call slot is currently connected to — the Kotlin port
 * of the web's muc-call-live-participants.ts.
 *
 * Why a separate store from `MucCallPresence.participants`: the Muji
 * presence view is right for rooms we are NOT in (every client sees
 * the same broadcast), but for the room we ARE in the SFU fires
 * participant connect/disconnect within seconds — before server-side
 * ping detects a dead resource — so the in-call roster prefers it.
 *
 * Empty list and missing room are equivalent: no entry means "fall
 * back to the Muji view". Identities are normalized with
 * [fullJidIdentityKey] for case-insensitive equality.
 */
class MucCallLiveParticipantsStore {
    private val _participants = MutableStateFlow<Map<String, List<String>>>(emptyMap())

    /** room → LiveKit participant identities (full JIDs), deduped. */
    val participants: StateFlow<Map<String, List<String>>> = _participants.asStateFlow()

    private val _leavingRooms = MutableStateFlow<Map<String, String>>(emptyMap())

    /**
     * Transient "we are leaving this room's call" markers (room →
     * self-nick). While set, the resolved roster excludes the marked
     * nick from the stale Muji fallback so a local leave counts down
     * monotonically instead of bouncing 0 → N → 0 for one RTT.
     */
    val leavingRooms: StateFlow<Map<String, String>> = _leavingRooms.asStateFlow()

    /**
     * Replace the room's whole participant snapshot. Empty input drops
     * the entry (fall back to Muji); a non-empty snapshot proves we are
     * (re)connected, so any pending leave marker is stale and cleared.
     */
    fun setParticipants(roomJid: String, identities: List<String>) {
        val room = normalizeCallRoomJid(roomJid)
        if (room.isEmpty()) return
        val deduped = identities.map(::fullJidIdentityKey).filter { it.isNotEmpty() }.distinct()
        if (deduped.isEmpty()) {
            clearRoom(room)
            return
        }
        clearLeaving(room)
        _participants.value = _participants.value + (room to deduped)
    }

    /** Drop the room's snapshot so the Muji-derived view re-engages. */
    fun clearRoom(roomJid: String) {
        val room = normalizeCallRoomJid(roomJid)
        if (room.isEmpty() || room !in _participants.value) return
        _participants.value = _participants.value - room
    }

    /** Mark [roomJid] locally leaving; no-op without a nick. */
    fun markLeaving(roomJid: String, selfNick: String?) {
        val room = normalizeCallRoomJid(roomJid)
        if (room.isEmpty() || selfNick.isNullOrEmpty()) return
        _leavingRooms.value = _leavingRooms.value + (room to selfNick)
    }

    /** Consume the room's leave marker (Muji caught up, or a fresh join). */
    fun clearLeaving(roomJid: String) {
        val room = normalizeCallRoomJid(roomJid)
        if (room.isEmpty() || room !in _leavingRooms.value) return
        _leavingRooms.value = _leavingRooms.value - room
    }
}
