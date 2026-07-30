package social.waddle.android.feature.conversation

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update
import social.waddle.android.client.MessageSendExtras
import social.waddle.android.client.SendResult
import social.waddle.client.ffi.WaddleSendMessageOutcome

/**
 * Optimistic pending sends: the local rows shown before the server echo
 * arrives, their lifecycle transitions (sent/queued/acked/failed), and
 * the ack/failure races against the in-flight send continuation.
 */
class PendingSendTracker {
    private val _pending = MutableStateFlow<List<PendingMessage>>(emptyList())

    /** Unconfirmed rows in append order. */
    val pending: StateFlow<List<PendingMessage>> = _pending

    private var nextLocalId = 0L

    // Delivery events can beat the send continuation, so ids are kept
    // until their row is pruned as stored — and bounded (oldest-evicted)
    // so ids that never match a row cannot accumulate for the screen's
    // lifetime.
    private val ackedIds = linkedSetOf<String>()
    private val failedIds = linkedSetOf<String>()

    // The timeline can receive the local DM projection while the send
    // continuation is still suspended below the manager. Keep its latest
    // identities here so an ack that arrives later can reveal that stored
    // row immediately, rather than waiting for another timeline emission.
    private var storedIds: Set<String> = emptySet()

    /** Append an optimistic row for a send about to dispatch. */
    fun append(body: String, extras: MessageSendExtras?, timestampMillis: Long): PendingMessage {
        val message = PendingMessage(
            localId = nextLocalId++,
            stanzaId = null,
            body = body,
            timestampMillis = timestampMillis,
            failed = false,
            extras = extras,
        )
        _pending.update { it + message }
        return message
    }

    /**
     * Adopt the send outcome for [localId]: a `Sent` outcome adopts the
     * returned stanza id; a QUEUED failure (the manager persisted the
     * message for replay — [SendResult.queuedId]) adopts the queue id,
     * which the replay reuses as its XEP-0359 origin-id, so the eventual
     * echo collapses this row and delivery events target it, exactly
     * like a live send. Only non-queued (permanent) outcomes mark the
     * row failed. Returns true when the send is tracked (sent or queued).
     */
    fun onSendResult(localId: Long, result: SendResult): Boolean {
        val trackedId = when (val outcome = result.outcome) {
            is WaddleSendMessageOutcome.Sent -> outcome.stanzaId
            else -> result.queuedId
        }
        if (trackedId == null) {
            updatePending(localId) { it.copy(failed = true) }
            return false
        }
        updatePending(localId) {
            it.copy(
                stanzaId = trackedId,
                queued = result.queued && trackedId !in failedIds,
                // Both the ack AND the failure event can beat this
                // continuation; failure wins.
                acked = trackedId in ackedIds && trackedId !in failedIds,
                failed = trackedId in failedIds,
            )
        }
        removeAcknowledgedStoredRows()
        return true
    }

    /**
     * The 0198 ack carries the client-generated id the send returned.
     * The row is only MARKED acked, never removed: a DM has no
     * reflection back to the sending resource, so deleting here would
     * vanish the message until the next MAM refetch. MUC rows disappear
     * when the stored echo matches an identity id.
     */
    fun onDeliveryAcked(stanzaId: String) {
        remember(ackedIds, stanzaId)
        _pending.update { list ->
            list.map {
                if (it.stanzaId == stanzaId) it.copy(acked = true, failed = false, queued = false) else it
            }
        }
        removeAcknowledgedStoredRows()
    }

    fun onDeliveryFailed(stanzaId: String) {
        remember(failedIds, stanzaId)
        _pending.update { list ->
            list.map {
                if (it.stanzaId == stanzaId) it.copy(failed = true, queued = false) else it
            }
        }
    }

    /**
     * Remember the latest stored identities and prune only an ACKED pending
     * row. A local DM echo can land before [onSendResult] adopts its
     * XEP-0359 origin-id; the optimistic row owns the visible delivery state
     * until the exact XEP-0198 acknowledgement, so a stored echo alone is
     * never authority to hide it as delivered.
     */
    fun pruneAgainst(storedIds: Set<String>) {
        this.storedIds = storedIds
        removeAcknowledgedStoredRows()
    }

    /**
     * Remove and return the failed row [localId] so the caller can
     * re-append and re-dispatch it; `null` when the row is missing or
     * not failed.
     */
    fun takeRetry(localId: Long): PendingMessage? {
        val message = _pending.value.firstOrNull { it.localId == localId && it.failed } ?: return null
        _pending.update { list -> list.filterNot { it.localId == localId } }
        return message
    }

    private fun updatePending(localId: Long, transform: (PendingMessage) -> PendingMessage) {
        _pending.update { list ->
            list.map { if (it.localId == localId) transform(it) else it }
        }
    }

    /**
     * Acknowledgement is the durable ownership transfer. Only then may a
     * stored local echo replace its optimistic overlay; DMs without a stored
     * reflection deliberately retain their acked optimistic row.
     */
    private fun removeAcknowledgedStoredRows() {
        val settled = _pending.value.asSequence()
            .filter { it.acked && it.stanzaId != null && it.stanzaId in storedIds }
            .mapNotNull { it.stanzaId }
            .toSet()
        if (settled.isEmpty()) return
        ackedIds -= settled
        failedIds -= settled
        _pending.update { list ->
            list.filterNot { it.acked && it.stanzaId != null && it.stanzaId in storedIds }
        }
    }

    private fun remember(ids: LinkedHashSet<String>, id: String) {
        ids.remove(id)
        ids.add(id)
        while (ids.size > MAX_TRACKED_DELIVERY_IDS) ids.remove(ids.first())
    }

    private companion object {
        /** Far above any realistic in-flight send count. */
        const val MAX_TRACKED_DELIVERY_IDS = 256
    }
}
