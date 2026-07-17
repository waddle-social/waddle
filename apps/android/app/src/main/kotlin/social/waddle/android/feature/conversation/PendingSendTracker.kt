package social.waddle.android.feature.conversation

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update
import social.waddle.android.client.DeliveryOutcomeRef
import social.waddle.android.client.MessageSendExtras
import social.waddle.android.client.SendResult
import social.waddle.android.client.prefs.DeliveryRowIdentity
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
    private val ackedDeliveries = linkedSetOf<DeliveryRowIdentity>()
    private val failedDeliveries = linkedSetOf<DeliveryRowIdentity>()

    /** Append an optimistic row for a send about to dispatch. */
    fun append(body: String, extras: MessageSendExtras?, timestampMillis: Long): PendingMessage {
        val message = PendingMessage(
            localId = nextLocalId++,
            stanzaId = null,
            delivery = null,
            body = body,
            timestampMillis = timestampMillis,
            failed = false,
            extras = extras,
        )
        _pending.update { it + message }
        return message
    }

    /**
     * Adopt the send outcome for [localId]. Every accepted send carries the
     * exact durable row identity; queued replay reuses its client stanza id,
     * which the replay reuses as its XEP-0359 origin-id, so the eventual
     * echo collapses this row and delivery events target it, exactly
     * like a live send. Only non-queued (permanent) outcomes mark the
     * row failed. Returns true when the send is tracked (sent or queued).
     */
    fun onSendResult(localId: Long, result: SendResult): Boolean {
        val delivery = result.delivery
        if (delivery == null) {
            updatePending(localId) { it.copy(failed = true) }
            return false
        }
        val identity = delivery.identity
        val trackedId = identity.clientStanzaId
        val sent = result.outcome as? WaddleSendMessageOutcome.Sent
        if (sent != null && sent.stanzaId != trackedId) {
            updatePending(localId) { it.copy(failed = true) }
            return false
        }
        updatePending(localId) {
            it.copy(
                stanzaId = trackedId,
                delivery = delivery,
                queued = result.queued && identity !in failedDeliveries,
                // Both the ack AND the failure event can beat this
                // continuation; failure wins.
                acked = identity in ackedDeliveries && identity !in failedDeliveries,
                failed = identity in failedDeliveries,
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
    fun onDeliveryAcked(delivery: DeliveryOutcomeRef) {
        remember(ackedDeliveries, delivery.identity)
        _pending.update { list ->
            list.map {
                if (it.delivery?.identity == delivery.identity) {
                    it.copy(acked = true, failed = false, queued = false)
                } else {
                    it
                }
            }
        }
    }

    fun onDeliveryFailed(delivery: DeliveryOutcomeRef) {
        remember(failedDeliveries, delivery.identity)
        _pending.update { list ->
            list.map {
                if (it.delivery?.identity == delivery.identity) {
                    it.copy(failed = true, queued = false)
                } else {
                    it
                }
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
        // A stored row's races are settled: its delivery ids are done.
        // Removal happens OUTSIDE the update lambda (CAS retries must
        // stay side-effect free).
        val settled = _pending.value
            .filter { it.stanzaId != null && it.stanzaId in storedIds }
            .mapNotNullTo(mutableSetOf()) { it.delivery?.identity }
        ackedDeliveries -= settled
        failedDeliveries -= settled
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

    private fun remember(
        identities: LinkedHashSet<DeliveryRowIdentity>,
        identity: DeliveryRowIdentity,
    ) {
        identities.remove(identity)
        identities.add(identity)
        while (identities.size > MAX_TRACKED_DELIVERIES) {
            identities.remove(identities.first())
        }
    }

    private companion object {
        /** Far above any realistic in-flight send count. */
        const val MAX_TRACKED_DELIVERIES = 256
    }
}
