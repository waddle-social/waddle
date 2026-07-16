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
import social.waddle.android.client.prefs.SmResumeSnapshot
import social.waddle.android.client.prefs.toSnapshot
import social.waddle.client.ffi.WaddleSmResumeState

/**
 * Persists the XEP-0198 resume snapshot and the per-conversation
 * catch-up cursors behind conflated channels: [queueResumeSnapshot]
 * arrives on Rust threads and never blocks, [recordCursor] never blocks
 * the event fan-out, and the session-scoped persister loops coalesce
 * bursts into single DataStore writes. Re-armed per login via [start].
 */
internal class ResumePersistence(
    private val sessionPrefs: SessionPrefs,
) {
    /** In-memory newest-seen cursors ranking the bounded DM catch-up. */
    val cursorTracker = ResumeCursorTracker()

    @Volatile
    private var resumeSnapshots: Channel<ResumeUpdate> = Channel(Channel.CONFLATED)

    /** Conflated "cursors changed" ticks; the persister coalesces bursts. */
    @Volatile
    private var cursorWrites: Channel<Unit> = Channel(Channel.CONFLATED)

    /** Fresh channels + persister loops on [scope], once per login. */
    fun start(scope: CoroutineScope) {
        resumeSnapshots = Channel(Channel.CONFLATED)
        cursorWrites = Channel(Channel.CONFLATED)
        scope.launch { persistResumeSnapshots(resumeSnapshots) }
        scope.launch { persistResumeCursors(cursorWrites) }
    }

    /** Called from Rust threads via the bridge: never blocks. */
    fun queueResumeSnapshot(state: WaddleSmResumeState?) {
        resumeSnapshots.trySend(ResumeUpdate(state?.toSnapshot()))
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

    private suspend fun persistResumeSnapshots(updates: ReceiveChannel<ResumeUpdate>) {
        for (update in updates) {
            persistQuietly { sessionPrefs.setSmResume(update.snapshot) }
        }
    }

    private suspend fun persistResumeCursors(writes: ReceiveChannel<Unit>) {
        while (writes.receiveCatching().isSuccess) {
            persistQuietly { sessionPrefs.setResumeCursors(cursorTracker.snapshot()) }
        }
    }

    /** Wrapper so a conflated channel can carry a `null` (= clear) update. */
    private data class ResumeUpdate(val snapshot: SmResumeSnapshot?)
}
