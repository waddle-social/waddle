package social.waddle.android.client.store

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.getAndUpdate
import social.waddle.android.client.bareJid

/**
 * XEP-0490 read cursors: per conversation, the XEP-0359 stanza id of
 * the latest message this account displayed on ANY device. Written by
 * this device's own displayed dispatches and by MDS entries from
 * sibling devices; equality-deduped here, ordering (does the new
 * cursor actually advance?) is the caller's job because only the
 * timeline knows message order.
 */
class ReadCursorStore {
    private val _cursors = MutableStateFlow<Map<String, String>>(emptyMap())

    /** conversation bare JID → displayed stanza id. */
    val cursors: StateFlow<Map<String, String>> = _cursors.asStateFlow()

    fun cursor(conversationJid: String): String? = _cursors.value[bareJid(conversationJid)]

    /** Record [stanzaId] as displayed; false when already the cursor. */
    fun advance(conversationJid: String, stanzaId: String): Boolean {
        val conversation = bareJid(conversationJid)
        // CAS: writers race from the UI scope, the event consumer, and
        // the MDS bootstrap — plain read-modify-write loses updates.
        val previous = _cursors.getAndUpdate { it + (conversation to stanzaId) }
        return previous[conversation] != stanzaId
    }

    /**
     * Advance only if the cursor is still [expected] — callers that
     * computed ordering from a snapshot use this so a concurrent local
     * advance cannot be regressed by a stale write.
     */
    fun compareAndAdvance(conversationJid: String, expected: String?, stanzaId: String): Boolean {
        val conversation = bareJid(conversationJid)
        var swapped = false
        _cursors.getAndUpdate { cursors ->
            if (cursors[conversation] == expected) {
                swapped = true
                cursors + (conversation to stanzaId)
            } else {
                swapped = false
                cursors
            }
        }
        return swapped
    }

    fun clear() {
        _cursors.value = emptyMap()
    }
}
