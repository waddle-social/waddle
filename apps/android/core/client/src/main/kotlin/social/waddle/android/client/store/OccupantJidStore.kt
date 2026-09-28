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

    /** room → OUR actual occupant nick (self-presence, status 110). */
    private val ownNicks = HashMap<String, String>()

    /**
     * Our actual nick in [roomJid] per the room's self-presence (XEP-0045
     * status 110, including a 210 room-assigned rename), or `null` when
     * we have none this session.
     */
    fun ownNickIn(roomJid: String): String? = synchronized(ownNicks) { ownNicks[normalizedBareJid(roomJid)] }

    /** Who is behind `roomJid/nick` right now, if known. */
    fun jidFor(roomJid: String, nick: String): String? = jids.value[normalizedBareJid(roomJid)]?.get(nick)

    fun onPresence(presence: WaddlePresence) {
        val from = presence.from ?: return
        val nick = resourcepart(from) ?: return
        val realJid = presence.mucJid?.let(::normalizedBareJid)?.takeIf { '@' in it }
        val room = normalizedBareJid(from)
        if (SELF_PRESENCE in presence.mucStatusCodes) trackOwnNick(room, nick, presence.presenceType)
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
        synchronized(ownNicks) { ownNicks.clear() }
    }

    private fun trackOwnNick(room: String, nick: String, presenceType: String) {
        synchronized(ownNicks) {
            if (presenceType !in NOT_AVAILABLE) {
                ownNicks[room] = nick
            } else if (ownNicks[room] == nick) {
                // We left (or are changing nick; the new self-presence follows).
                ownNicks -= room
            }
        }
    }

    private companion object {
        val NOT_AVAILABLE = setOf("unavailable", "error")

        /** XEP-0045 status 110: this presence is about the recipient itself. */
        const val SELF_PRESENCE: UShort = 110u
    }
}

/**
 * The real bare JID behind a timeline row's author, or `null` when it
 * is unknown (the row then renders initials — a wrong face is worse).
 * Room rows resolve ONLY through the JID stamped when stored
 * ([TimelineItem.authorJid]) — never the nick-based mine flag. Our own
 * reflections are stamped at ingest only when the room's self-presence
 * verified the nick; delayed history, archive rows and anything that
 * merely matches our configured nick stay unknown. 1:1 rows are the
 * account or the sender.
 */
fun authorBareJidOf(item: TimelineItem, ownBareJid: String?): String? {
    val isGroupchat = when (val source = item.source) {
        is TimelineSource.Live -> source.message.isMuc || source.message.messageType == "groupchat"
        is TimelineSource.Archived -> source.message.messageType == "groupchat"
    }
    val own = ownBareJid?.let(::normalizedBareJid)
    // Room rows: the stored stamp wins over the nick-based mine flag.
    if (isGroupchat) return item.authorJid
    if (item.isMine && own != null) return own
    return item.from?.let(::normalizedBareJid)
}
