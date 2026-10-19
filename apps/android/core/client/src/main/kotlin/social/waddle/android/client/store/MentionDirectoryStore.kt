package social.waddle.android.client.store

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.update
import social.waddle.android.client.normalizedBareJid
import social.waddle.client.ffi.WaddleUserSearchEntry

/**
 * XEP-0055 directory users seen by a room's `@` mention searches, so a
 * typed `@username` of someone with no MUC presence still resolves to a
 * XEP-0372 reference at send time. Keyed by normalised room JID, then
 * normalised user bare JID; the first username recorded for a JID
 * stays. In memory for the session; wiped via [SessionStores.clear].
 */
class MentionDirectoryStore {
    private val rooms = MutableStateFlow<Map<String, Map<String, WaddleUserSearchEntry>>>(emptyMap())

    /** Remember [entries] as mentionable in [roomJid]. */
    fun record(roomJid: String, entries: List<WaddleUserSearchEntry>) {
        if (entries.isEmpty()) return
        val room = normalizedBareJid(roomJid)
        rooms.update { current ->
            val seen = LinkedHashMap(current[room].orEmpty())
            entries.forEach { entry ->
                val jid = normalizedBareJid(entry.jid)
                if (jid.contains('@') && entry.username.isNotBlank() && jid !in seen) {
                    seen[jid] = entry.copy(jid = jid)
                }
            }
            current + (room to seen)
        }
    }

    /** Every directory user seen in [roomJid]'s searches, first-seen order. */
    fun usersIn(roomJid: String): List<WaddleUserSearchEntry> =
        rooms.value[normalizedBareJid(roomJid)]?.values?.toList().orEmpty()

    fun clear() {
        rooms.value = emptyMap()
    }
}
