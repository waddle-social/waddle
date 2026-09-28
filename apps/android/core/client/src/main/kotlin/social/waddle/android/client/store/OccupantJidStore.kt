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
 * Last-known MUC nick → real bare JID mapping per room, from occupant
 * presence carrying the XEP-0045 `<item jid=…>` (non-anonymous rooms).
 * Entries survive leaves so a message racing its author's
 * `unavailable` still resolves; a later presence re-points a reused nick.
 *
 * This is a "who is behind this nick NOW" lookup: it stamps live rows
 * at arrival and names typists — stored rows never resolve through it
 * (see [TimelineItem.authorJid]), and archive rows never feed it (an
 * old MAM author must not label a later occupant of the same nick).
 * The FFI exposes no XEP-0421 occupant id, so room + nick is the key.
 */
class OccupantJidStore {
    private val _jids = MutableStateFlow<Map<String, Map<String, String>>>(emptyMap())

    /** [normalizedBareJid] room → (nick → normalized real bare JID). */
    val jids: StateFlow<Map<String, Map<String, String>>> = _jids.asStateFlow()

    /** The current nick map of [roomJid] (any case). */
    fun jidsIn(roomJid: String): Flow<Map<String, String>> {
        val room = normalizedBareJid(roomJid)
        return jids.map { rooms -> rooms[room].orEmpty() }.distinctUntilChanged()
    }

    /** Who is behind `roomJid/nick` right now, if known. */
    fun jidFor(roomJid: String, nick: String): String? = jids.value[normalizedBareJid(roomJid)]?.get(nick)

    fun onPresence(presence: WaddlePresence) {
        val from = presence.from ?: return
        val nick = resourcepart(from) ?: return
        val realJid = presence.mucJid?.let(::normalizedBareJid)?.takeIf { '@' in it }
        val room = normalizedBareJid(from)
        when {
            realJid != null -> _jids.update { rooms ->
                val known = rooms[room].orEmpty()
                if (known[nick] == realJid) rooms else rooms + (room to known + (nick to realJid))
            }
            // An AVAILABLE occupant presence without a real JID (we were
            // demoted, the room went semi-anonymous, a remote MUC omits
            // it): whoever holds the nick now is unknown — forget the old
            // holder so they are not stamped onto new rows. Leaves keep
            // the entry (a message may race its author's unavailable).
            isOccupant(presence) && presence.presenceType !in NOT_AVAILABLE -> _jids.update { rooms ->
                val known = rooms[room] ?: return@update rooms
                if (nick !in known) rooms else rooms + (room to known - nick)
            }
        }
    }

    private fun isOccupant(presence: WaddlePresence): Boolean =
        presence.mucRole != null ||
            presence.mucAffiliation != null ||
            presence.mucStatusCodes.isNotEmpty()

    fun clear() {
        _jids.value = emptyMap()
    }

    private companion object {
        val NOT_AVAILABLE = setOf("unavailable", "error")
    }
}

/**
 * The real bare JID behind a timeline row's author, or `null` when it
 * is unknown (the row then renders initials — a wrong face is worse).
 * Own rows are the account; 1:1 rows the sender; room rows ONLY the
 * JID stamped on the row when it was stored ([TimelineItem.authorJid]).
 */
fun authorBareJidOf(item: TimelineItem, ownBareJid: String?): String? {
    if (item.isMine && ownBareJid != null) return normalizedBareJid(ownBareJid)
    val from = item.from ?: return null
    val isGroupchat = when (val source = item.source) {
        is TimelineSource.Live -> source.message.isMuc || source.message.messageType == "groupchat"
        is TimelineSource.Archived -> source.message.messageType == "groupchat"
    }
    return if (isGroupchat) item.authorJid else normalizedBareJid(from)
}
