package social.waddle.android.client.store

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.map
import social.waddle.android.client.bareJid
import social.waddle.client.ffi.WaddleChatState

/**
 * XEP-0085 incoming typing state, web parity: only `composing` is
 * meaningfully consumed — it adds the sender with a 5s auto-expiry;
 * every other state (paused/active/inactive/gone) removes immediately,
 * and a real message from the sender clears their typing too. Expiry
 * is enforced on [sweep], driven by the session manager's ticker while
 * anyone is composing.
 */
class ChatStateStore(private val clock: () -> Long = System::currentTimeMillis) {
    private val lock = Any()

    /** conversation → sender display name → expiry epoch millis. */
    private val composers = HashMap<String, MutableMap<String, Long>>()
    private val _composing = MutableStateFlow<Map<String, List<String>>>(emptyMap())

    /** conversation bare JID → currently-composing sender names. */
    val composing: StateFlow<Map<String, List<String>>> = _composing.asStateFlow()

    fun composingNames(conversationJid: String): Flow<List<String>> {
        val conversation = bareJid(conversationJid)
        return _composing.map { it[conversation].orEmpty() }
    }

    fun onChatState(conversationJid: String, sender: String, state: WaddleChatState, isMine: Boolean) {
        if (isMine) return
        synchronized(lock) {
            val conversation = bareJid(conversationJid)
            if (state == WaddleChatState.COMPOSING) {
                composers.getOrPut(conversation) { mutableMapOf() }[sender] = clock() + TYPING_EXPIRY_MILLIS
            } else {
                removeComposer(conversation, sender)
            }
            publish()
        }
    }

    /** A delivered message ends its sender's typing state. */
    fun onLiveMessage(conversationJid: String, sender: String) {
        synchronized(lock) {
            if (removeComposer(bareJid(conversationJid), sender)) publish()
        }
    }

    /** Never leaves an empty conversation entry: [composing]'s emptiness
     *  gates the caller's sweep ticker, so a stale empty map would keep
     *  a timer armed forever. */
    private fun removeComposer(conversation: String, sender: String): Boolean {
        val senders = composers[conversation] ?: return false
        val removed = senders.remove(sender) != null
        if (senders.isEmpty()) composers.remove(conversation)
        return removed
    }

    /**
     * Drop expired composers; returns true while anyone is still
     * composing (the caller keeps ticking only then).
     */
    fun sweep(): Boolean = synchronized(lock) {
        val now = clock()
        var changed = false
        composers.values.forEach { senders ->
            changed = senders.entries.removeAll { it.value <= now } || changed
        }
        composers.entries.removeAll { it.value.isEmpty() }
        if (changed) publish()
        composers.isNotEmpty()
    }

    fun clear() {
        synchronized(lock) {
            composers.clear()
            publish()
        }
    }

    private fun publish() {
        _composing.value = composers.mapValues { (_, senders) -> senders.keys.sorted() }
    }

    private companion object {
        /** Web TYPING_EXPIRY_MS parity. */
        const val TYPING_EXPIRY_MILLIS = 5_000L
    }
}
