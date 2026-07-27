package social.waddle.android.client.session

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.channels.ReceiveChannel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import social.waddle.android.client.ResumeCursorTracker
import social.waddle.android.client.nowRfc3339
import social.waddle.android.client.persistQuietly
import social.waddle.android.client.prefs.SessionPrefs

/**
 * Persists per-conversation catch-up cursors behind a conflated channel.
 * [recordCursor] never blocks event fan-out and the session-scoped persister
 * coalesces bursts into single DataStore writes. Re-armed per login via [start].
 */
internal class ResumePersistence(
    private val sessionPrefs: SessionPrefs,
) {
    /** In-memory newest-seen cursors ranking the bounded DM catch-up. */
    val cursorTracker = ResumeCursorTracker()

    /** Conflated "cursors changed" ticks; the persister coalesces bursts. */
    @Volatile
    private var cursorWrites = Channel<Unit>(Channel.CONFLATED)

    /** Fresh channels + persister loops on [scope], once per login. */
    fun start(scope: CoroutineScope) {
        cursorWrites = Channel(Channel.CONFLATED)
        scope.launch { persistResumeCursors(cursorWrites) }
    }

    /**
     * Advance-only cursor bookkeeping from the fan-out (never blocks):
     * a moved cursor pokes the conflated write channel and the session-
     * scoped persister flushes the snapshot — bursts coalesce into one
     * DataStore write.
     */
    fun recordCursor(conversationJid: String, stanzaId: String?, timestamp: String?) {
        stanzaId ?: return
        if (cursorTracker.advance(conversationJid, stanzaId, timestamp ?: nowRfc3339())) {
            cursorWrites.trySend(Unit)
        }
    }

    /** Restore the persisted cursors into the tracker (login). */
    suspend fun seedFromPrefs() {
        cursorTracker.seed(sessionPrefs.resumeCursors.first())
    }

    fun clear() {
        cursorTracker.clear()
    }

    private suspend fun persistResumeCursors(writes: ReceiveChannel<Unit>) {
        while (writes.receiveCatching().isSuccess) {
            persistQuietly { sessionPrefs.setResumeCursors(cursorTracker.snapshot()) }
        }
    }
}
