package social.waddle.android.client.store

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * Unread mentions per conversation: live messages that address the
 * account (an XEP-0372 reference to its bare JID, or a broadcast
 * `@everyone`/`@here`) and counted as unread.
 *
 * Read-only for everyone but [UnreadStore], which writes it under its
 * own lock in the same step as the unread count, so a mention is
 * counted exactly when its message is and cleared exactly when the
 * conversation's unread is (opening it, marking it displayed, a sibling
 * device's XEP-0490 cursor, a server inbox push reading zero).
 *
 * Live-only (web parity): the server inbox carries no mention counts,
 * so the counter is empty after every fresh stream (it resets on each
 * reconnect, as well as on sign-in and sign-out), a mention that
 * arrived while disconnected is never counted, and nothing is
 * persisted. The UI must never claim more than this knows. An absolute
 * unread recompute that leaves fewer unread messages than counted
 * mentions caps the mentions at that count.
 */
class MentionStore {
    private val _counts = MutableStateFlow<Map<String, Int>>(emptyMap())

    /** conversation bare JID → unread mentions (absent = zero). */
    val counts: StateFlow<Map<String, Int>> = _counts.asStateFlow()

    internal fun increment(conversation: String) {
        _counts.value += conversation to (_counts.value[conversation] ?: 0) + 1
    }

    internal fun clear(conversation: String) {
        if (conversation in _counts.value) _counts.value -= conversation
    }

    /** At most [unread] mentions can still be unread in [conversation]. */
    internal fun capAt(conversation: String, unread: Int) {
        val current = _counts.value[conversation] ?: return
        if (unread <= 0) {
            _counts.value -= conversation
        } else if (current > unread) {
            _counts.value += conversation to unread
        }
    }

    internal fun clearAll() {
        _counts.value = emptyMap()
    }
}
