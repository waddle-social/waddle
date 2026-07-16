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
    private val ackedIds = mutableSetOf<String>()
    private val failedIds = mutableSetOf<String>()

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
        ackedIds += stanzaId
        _pending.update { list ->
            list.map {
                if (it.stanzaId == stanzaId) it.copy(acked = true, failed = false, queued = false) else it
            }
        }
    }

    fun onDeliveryFailed(stanzaId: String) {
        failedIds += stanzaId
        _pending.update { list ->
            list.map {
                if (it.stanzaId == stanzaId) it.copy(failed = true, queued = false) else it
            }
        }
    }

    /**
     * Prune (not just hide) rows whose identity the timeline now holds:
     * a view-side filter alone lets the list grow for the screen's
     * lifetime, and a timeline trim could drop the stored row and
     * resurrect an already-delivered send as an unconfirmed ghost.
     */
    fun pruneAgainst(storedIds: Set<String>) {
        _pending.update { list ->
            list.filterNot { it.stanzaId != null && it.stanzaId in storedIds }
        }
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
}
