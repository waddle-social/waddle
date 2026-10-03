package social.waddle.android.client.store

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import social.waddle.android.client.bareJid
import social.waddle.android.client.hasBotHat
import social.waddle.android.client.resourcepart
import social.waddle.client.ffi.WaddlePresence

/**
 * Presence bookkeeping: MUC occupant presence per room JID (keyed by
 * nick, `unavailable` removes) and the latest presence per contact bare
 * JID for everything else.
 */
class PresenceStore {
    private val _occupants = MutableStateFlow<Map<String, Map<String, WaddlePresence>>>(emptyMap())

    /** room bare JID → (nick → latest presence). */
    val occupants: StateFlow<Map<String, Map<String, WaddlePresence>>> = _occupants.asStateFlow()

    private val _contacts = MutableStateFlow<Map<String, WaddlePresence>>(emptyMap())

    /** contact bare JID → latest presence (including `unavailable`). */
    val contacts: StateFlow<Map<String, WaddlePresence>> = _contacts.asStateFlow()

    private val _botJids = MutableStateFlow<Set<String>>(emptySet())

    /**
     * Real bare JIDs the server hatted `urn:waddle:hats:bot`, kept for
     * the session (web parity): a bot's room presence is lazy and comes
     * and goes, its identity does not.
     */
    val botJids: StateFlow<Set<String>> = _botJids.asStateFlow()

    fun onPresence(presence: WaddlePresence) {
        val from = presence.from ?: return
        val nick = resourcepart(from)
        if (isMucOccupant(presence) && nick != null) {
            presence.mucJid?.takeIf { hasBotHat(presence.hats) }
                ?.let { real -> _botJids.update { it + bareJid(real) } }
            updateOccupant(roomJid = bareJid(from), nick = nick, presence = presence)
        } else {
            _contacts.update { it + (bareJid(from) to presence) }
        }
    }

    fun clear() {
        _occupants.value = emptyMap()
        _contacts.value = emptyMap()
        _botJids.value = emptySet()
    }

    private fun updateOccupant(roomJid: String, nick: String, presence: WaddlePresence) {
        _occupants.update { rooms ->
            val occupants = rooms[roomJid] ?: emptyMap()
            val next =
                if (presence.presenceType == "unavailable") {
                    occupants - nick
                } else {
                    occupants + (nick to presence)
                }
            if (next.isEmpty()) rooms - roomJid else rooms + (roomJid to next)
        }
    }

    private fun isMucOccupant(presence: WaddlePresence): Boolean =
        presence.mucRole != null ||
            presence.mucAffiliation != null ||
            presence.mucJid != null ||
            presence.mucStatusCodes.isNotEmpty()
}
