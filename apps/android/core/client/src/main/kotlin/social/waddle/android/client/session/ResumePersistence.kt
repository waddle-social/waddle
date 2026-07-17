package social.waddle.android.client.session

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.channels.ReceiveChannel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import social.waddle.android.client.OutboundQueue
import social.waddle.android.client.ResumeCursorTracker
import social.waddle.android.client.nowRfc3339
import social.waddle.android.client.persistQuietly
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryAttemptTransition
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.SmResumeSnapshot
import social.waddle.android.client.prefs.toSnapshot
import social.waddle.client.ffi.WaddleSmResumeState
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong
import java.util.logging.Level
import java.util.logging.Logger

/**
 * Persists XEP-0198 resume snapshots in the serialized native-event lane
 * and keeps only per-conversation catch-up cursors behind a conflated
 * channel. Snapshot writes therefore complete before the next native event
 * is pulled, while [recordCursor] remains non-blocking and the
 * session-scoped cursor persister coalesces bursts into single DataStore
 * writes. Re-armed per login via [start].
 */
internal class ResumePersistence(
    private val sessionPrefs: SessionPrefs,
    private val deliveryJournal: OutboundQueue,
) {
    /** In-memory newest-seen cursors ranking the bounded DM catch-up. */
    val cursorTracker = ResumeCursorTracker()

    private val updateSequences =
        ConcurrentHashMap<DeliveryAttemptRef, AtomicLong>()

    /** Conflated "cursors changed" ticks; the persister coalesces bursts. */
    @Volatile
    private var cursorWrites: Channel<Unit> = Channel(Channel.CONFLATED)

    /** Fresh channels + persister loops on [scope], once per login. */
    fun start(scope: CoroutineScope) {
        cursorWrites = Channel(Channel.CONFLATED)
        updateSequences.clear()
        scope.launch { persistResumeCursors(cursorWrites) }
    }

    /** Register the durable attempt before its bridge can emit callbacks. */
    fun registerAttempt(attempt: DeliveryAttemptRef, persistedVersion: Long) {
        updateSequences[attempt] = AtomicLong(persistedVersion)
    }

    fun retireAttempt(attempt: DeliveryAttemptRef) {
        updateSequences.remove(attempt)
    }

    fun acceptResumeTransition(
        transition: DeliveryAttemptTransition,
        persistedVersion: Long,
    ): Boolean {
        updateSequences[transition.fresh]?.let { existing ->
            return existing.get() >= persistedVersion
        }
        val sequence = updateSequences.remove(transition.old) ?: return false
        sequence.updateAndGet { current -> maxOf(current, persistedVersion) }
        updateSequences[transition.fresh] = sequence
        return true
    }

    /**
     * Ordered pull-boundary persistence. The connection loop awaits this
     * exact attempt/version write before polling another native event.
     */
    suspend fun persistResumeSnapshot(
        attempt: DeliveryAttemptRef,
        state: WaddleSmResumeState?,
    ): Boolean {
        val sequence = updateSequences[attempt]?.incrementAndGet() ?: return false
        return persistResumeUpdate(
            ResumeUpdate(attempt, sequence, state?.toSnapshot()),
        )
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
        updateSequences.clear()
    }

    private suspend fun persistResumeUpdate(update: ResumeUpdate): Boolean {
        var retryIndex = 0
        while (true) {
            try {
                // false means the attempt/version is stale; retrying could
                // only threaten the replacement's tombstone.
                if (
                    !deliveryJournal.saveSmResume(
                        attempt = update.attempt,
                        version = update.sequence,
                        snapshot = update.snapshot,
                    )
                ) {
                    return false
                }
                return true
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (failure: Throwable) {
                LOGGER.log(Level.WARNING, "delivery journal SM update failed; retrying", failure)
                delay(RETRY_DELAYS_MILLIS[retryIndex.coerceAtMost(RETRY_DELAYS_MILLIS.lastIndex)])
                if (retryIndex < RETRY_DELAYS_MILLIS.lastIndex) retryIndex += 1
            }
        }
    }

    private suspend fun persistResumeCursors(writes: ReceiveChannel<Unit>) {
        while (writes.receiveCatching().isSuccess) {
            persistQuietly { sessionPrefs.setResumeCursors(cursorTracker.snapshot()) }
        }
    }

    /** Wrapper so a conflated channel can carry a `null` (= clear) update. */
    private data class ResumeUpdate(
        val attempt: DeliveryAttemptRef,
        val sequence: Long,
        val snapshot: SmResumeSnapshot?,
    )

    private companion object {
        val LOGGER: Logger = Logger.getLogger(ResumePersistence::class.java.name)
        val RETRY_DELAYS_MILLIS = longArrayOf(250L, 500L, 1_000L, 2_000L, 5_000L)
    }
}
