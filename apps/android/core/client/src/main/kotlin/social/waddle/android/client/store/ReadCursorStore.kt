package social.waddle.android.client.store

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
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
        if (_cursors.value[conversation] == stanzaId) return false
        _cursors.value = _cursors.value + (conversation to stanzaId)
        return true
    }

    fun clear() {
        _cursors.value = emptyMap()
    }
}
