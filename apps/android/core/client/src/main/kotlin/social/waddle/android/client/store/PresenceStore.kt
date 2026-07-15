package social.waddle.android.client.store

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import social.waddle.android.client.bareJid
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

    fun onPresence(presence: WaddlePresence) {
        val from = presence.from ?: return
        val nick = resourcepart(from)
        if (isMucOccupant(presence) && nick != null) {
            updateOccupant(roomJid = bareJid(from), nick = nick, presence = presence)
        } else {
            _contacts.update { it + (bareJid(from) to presence) }
        }
    }

    fun clear() {
        _occupants.value = emptyMap()
        _contacts.value = emptyMap()
    }

    private fun updateOccupant(roomJid: String, nick: String, presence: WaddlePresence) {
        _occupants.update { rooms ->
            val occupants = rooms[roomJid] ?: emptyMap()
            val next =
                if (presence.presenceType == "unavailable") occupants - nick
                else occupants + (nick to presence)
            if (next.isEmpty()) rooms - roomJid else rooms + (roomJid to next)
        }
    }

    private fun isMucOccupant(presence: WaddlePresence): Boolean =
        presence.mucRole != null ||
            presence.mucAffiliation != null ||
            presence.mucJid != null ||
            presence.mucStatusCodes.isNotEmpty()
}
