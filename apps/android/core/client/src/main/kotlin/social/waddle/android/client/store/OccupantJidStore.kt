package social.waddle.android.client.store

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update
import social.waddle.android.client.normalizedBareJid
import social.waddle.android.client.resourcepart
import social.waddle.client.ffi.WaddlePresence

/**
 * Last-known MUC nick → real bare JID mapping per room, so a timeline
 * author keeps their identity (and avatar) after leaving the room.
 *
 * Sources, strongest first: occupant presence carrying the XEP-0045
 * `<item jid=…>` (non-anonymous rooms; always overwrites), then the
 * archived `muc#user` real JID of a MAM row (fills gaps only — an old
 * row must not re-point a nick that live presence already resolved).
 * The FFI exposes no XEP-0421 occupant id, so room + nick is the key.
 * Nothing is ever guessed from a nick.
 */
class OccupantJidStore {
    private val _jids = MutableStateFlow<Map<String, Map<String, String>>>(emptyMap())

    /**
     * [normalizedBareJid] room → (nick → normalized real bare JID);
     * entries survive leaves. Read one room via [jidsIn].
     */
    val jids: StateFlow<Map<String, Map<String, String>>> = _jids.asStateFlow()

    /** The retained nick map of [roomJid] (any case). */
    fun jidsIn(roomJid: String): Flow<Map<String, String>> {
        val room = normalizedBareJid(roomJid)
        return jids.map { rooms -> rooms[room].orEmpty() }.distinctUntilChanged()
    }

    fun onPresence(presence: WaddlePresence) {
        val from = presence.from ?: return
        val nick = resourcepart(from) ?: return
        val realJid = presence.mucJid?.let(::realBareJid) ?: return
        record(normalizedBareJid(from), nick, realJid, overwrite = true)
    }

    /** A MAM row's archived real author JID for `room/nick`. */
    fun onArchivedAuthor(occupantJid: String, authorRealJid: String) {
        val nick = resourcepart(occupantJid) ?: return
        val realJid = realBareJid(authorRealJid) ?: return
        record(normalizedBareJid(occupantJid), nick, realJid, overwrite = false)
    }

    fun clear() {
        _jids.value = emptyMap()
    }

    private fun record(room: String, nick: String, realJid: String, overwrite: Boolean) {
        _jids.update { rooms ->
            val known = rooms[room].orEmpty()
            if (known[nick] == realJid || (!overwrite && nick in known)) {
                rooms
            } else {
                rooms + (room to known + (nick to realJid))
            }
        }
    }

    private fun realBareJid(jid: String): String? = normalizedBareJid(jid).takeIf { '@' in it }
}

/**
 * The real bare JID behind a timeline row's author, or `null` when it
 * is unknown (the row then renders initials — a wrong face is worse).
 * Returned JIDs are [normalizedBareJid]. 1:1 rows are authored by the sender's bare JID; room rows resolve via
 * the archived real JID, then the retained [occupantJids] (nick → JID).
 */
fun authorBareJidOf(
    item: TimelineItem,
    occupantJids: Map<String, String>,
    ownBareJid: String?,
): String? {
    if (item.isMine && ownBareJid != null) return normalizedBareJid(ownBareJid)
    val from = item.from ?: return null
    val isGroupchat = when (val source = item.source) {
        is TimelineSource.Live -> source.message.isMuc || source.message.messageType == "groupchat"
        is TimelineSource.Archived -> source.message.messageType == "groupchat"
    }
    if (!isGroupchat) return normalizedBareJid(from)
    (item.source as? TimelineSource.Archived)?.message?.authorRealJid
        ?.let(::normalizedBareJid)
        ?.takeIf { '@' in it }
        ?.let { return it }
    val nick = resourcepart(from) ?: return null
    return occupantJids[nick]
}
