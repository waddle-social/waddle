package social.waddle.android.client

import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.withContext
import social.waddle.android.client.prefs.NativeOutboundPhase
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.client.ffi.WaddleSendMessageOutcome

/**
 * Durable outbound journal (web `waddle.chat.outbound-queue` parity).
 * [OutboundOwnership.Ready] rows are eligible for Kotlin replay;
 * [OutboundOwnership.NativeOwned] rows belong to one exact connection
 * generation until XEP-0198 acknowledgement/failure or reconnect
 * reconciliation. This keeps uncertain sends through process death.
 */
class OutboundQueue(
    private val sessionPrefs: SessionPrefs,
    private val capacity: Int = DEFAULT_CAPACITY,
) {
    data class EnqueueResult(
        val stored: Boolean,
        val evicted: QueuedOutboundMessage? = null,
    )

    /**
     * Persist [message]; an existing id is replaced. At [capacity], only
     * the oldest [OutboundOwnership.Ready] row may be evicted. If every
     * row is native-owned, persistence fails closed instead of discarding
     * uncertain work.
     */
    suspend fun enqueue(message: QueuedOutboundMessage): EnqueueResult {
        return persist(message)
    }

    /** Atomically persist a new caller-owned send under its exact native
     * generation. This closes the Ready-row race with the concurrent
     * SessionReady drain: no other sender can claim the row between its
     * durable insert and the originating FFI call. */
    suspend fun enqueueClaimed(
        message: QueuedOutboundMessage,
        ownership: OutboundOwnership.NativeOwned,
    ): EnqueueResult = persist(message.copy(ownership = ownership))

    private suspend fun persist(message: QueuedOutboundMessage): EnqueueResult {
        var evicted: QueuedOutboundMessage? = null
        var stored = false
        sessionPrefs.updateOutboundQueue { current ->
            val withoutReplaced = current.filterNot { it.clientStanzaId == message.clientStanzaId }
            if (withoutReplaced.size >= capacity) {
                val evictedIndex = withoutReplaced.indexOfFirst {
                    it.ownership == OutboundOwnership.Ready
                }
                if (evictedIndex < 0) {
                    current
                } else {
                    evicted = withoutReplaced[evictedIndex]
                    stored = true
                    withoutReplaced.filterIndexed { index, _ -> index != evictedIndex } + message
                }
            } else {
                stored = true
                withoutReplaced + message
            }
        }
        return EnqueueResult(stored = stored, evicted = evicted)
    }

    /** Crash/reconnect reconciliation for one exact connection generation.
     * Rows present in the SM snapshot remain native-owned for resume replay;
     * every other stale generation becomes browser/Kotlin replayable again. */
    suspend fun reconcileAttempt(
        ownerBareJid: String,
        connectionGeneration: Long,
        resumeStanzaIds: Set<String>,
    ) {
        withContext(NonCancellable) {
            sessionPrefs.updateOutboundQueue { current ->
                current
                    .filter { it.ownerBareJid == ownerBareJid }
                    .map { row ->
                        if (row.clientStanzaId in resumeStanzaIds) {
                            row.copy(
                                ownership = OutboundOwnership.NativeOwned(
                                    connectionGeneration = connectionGeneration,
                                    phase = NativeOutboundPhase.RESUME,
                                ),
                            )
                        } else if (row.ownership is OutboundOwnership.NativeOwned) {
                            row.copy(ownership = OutboundOwnership.Ready)
                        } else {
                            row
                        }
                    }
            }
        }
    }

    suspend fun claimReady(
        clientStanzaId: String,
        ownership: OutboundOwnership.NativeOwned,
    ): QueuedOutboundMessage? {
        var claimed: QueuedOutboundMessage? = null
        sessionPrefs.updateOutboundQueue { current ->
            current.map { row ->
                if (row.clientStanzaId == clientStanzaId && row.ownership == OutboundOwnership.Ready) {
                    row.copy(ownership = ownership).also { claimed = it }
                } else {
                    row
                }
            }
        }
        return claimed
    }

    suspend fun release(
        clientStanzaId: String,
        expected: OutboundOwnership.NativeOwned,
    ): Boolean = transition(clientStanzaId, expected, OutboundOwnership.Ready)

    suspend fun transition(
        clientStanzaId: String,
        expected: OutboundOwnership,
        next: OutboundOwnership,
    ): Boolean {
        var changed = false
        withContext(NonCancellable) {
            sessionPrefs.updateOutboundQueue { current ->
                current.map { row ->
                    if (row.clientStanzaId == clientStanzaId && row.ownership == expected) {
                        changed = true
                        row.copy(ownership = next)
                    } else {
                        row
                    }
                }
            }
        }
        return changed
    }

    /** Delete only the row owned by the exact generation that produced the
     * acknowledgement. The durable commit completes before callers publish
     * `DeliveryAcked`. */
    suspend fun acknowledge(clientStanzaId: String, connectionGeneration: Long): Boolean {
        var removed = false
        withContext(NonCancellable) {
            sessionPrefs.updateOutboundQueue { current ->
                current.filterNot { row ->
                    val owned = row.ownership as? OutboundOwnership.NativeOwned
                    val matches = row.clientStanzaId == clientStanzaId &&
                        owned?.connectionGeneration == connectionGeneration
                    if (matches) removed = true
                    matches
                }
            }
        }
        return removed
    }

    /** First resume failure transfers the row to native fresh-stream
     * fallback. A later native failure releases it for a future replay. */
    suspend fun failNative(clientStanzaId: String, connectionGeneration: Long): FailureResolution {
        var resolution = FailureResolution.STALE
        withContext(NonCancellable) {
            sessionPrefs.updateOutboundQueue { current ->
                current.map { row ->
                    val owned = row.ownership as? OutboundOwnership.NativeOwned
                    if (
                        row.clientStanzaId != clientStanzaId ||
                        owned?.connectionGeneration != connectionGeneration
                    ) {
                        row
                    } else if (owned.phase == NativeOutboundPhase.RESUME) {
                        resolution = FailureResolution.TRANSFERRED_TO_FALLBACK
                        row.copy(
                            ownership = owned.copy(phase = NativeOutboundPhase.FALLBACK),
                        )
                    } else {
                        resolution = FailureResolution.RELEASED
                        row.copy(ownership = OutboundOwnership.Ready)
                    }
                }
            }
        }
        return resolution
    }

    /**
     * Replay the queue head-first through [send]. Per-message outcomes:
     * - `Sent` → retain under the exact native generation until a real
     *   XEP-0198 acknowledgement deletes it. A socket write is not a
     *   durable acknowledgement.
     * - `NotConnected` / `TransportError` → keep and STOP: the session
     *   is gone again, the remainder retries on the next `SessionReady`.
     * - anything else (`InvalidRecipient`, `InvalidOptions`,
     *   `StanzaError`, `Error`) → drop and report via [onDropped]: the
     *   session was live and rejected this exact payload, so replaying
     *   it can only fail the same way forever.
     *
     * No lock is held across [send] (an FFI call): each iteration
     * re-reads the persisted head, so concurrent enqueues interleave
     * safely. Every ownership transition and terminal delete is a
     * non-cancellable DataStore commit; reconnect reconciliation then
     * either transfers an SM-backed row or releases a stale claim.
     */
    suspend fun drain(
        ownerBareJid: String,
        connectionGeneration: Long,
        send: suspend (QueuedOutboundMessage) -> WaddleSendMessageOutcome,
        onDropped: suspend (QueuedOutboundMessage, WaddleSendMessageOutcome) -> Unit,
    ) {
        // Cross-account guard: drop anything another account enqueued
        // (e.g. a send committed in the logout teardown window) BEFORE
        // draining — replaying it here would misdeliver it under the
        // current account.
        while (true) {
            val ready = sessionPrefs.outboundQueue.first()
                .firstOrNull { it.ownerBareJid == ownerBareJid && it.ownership == OutboundOwnership.Ready }
                ?: return
            val ownership = OutboundOwnership.NativeOwned(
                connectionGeneration = connectionGeneration,
                phase = NativeOutboundPhase.FRESH,
            )
            val claimed = claimReady(ready.clientStanzaId, ownership) ?: continue
            when (val outcome = send(claimed)) {
                is WaddleSendMessageOutcome.Sent -> Unit
                WaddleSendMessageOutcome.NotConnected,
                WaddleSendMessageOutcome.TransportError,
                -> {
                    release(claimed.clientStanzaId, ownership)
                    return
                }
                else -> {
                    if (removeOwned(claimed.clientStanzaId, ownership)) {
                        onDropped(claimed, outcome)
                    }
                }
            }
        }
    }

    suspend fun removeOwned(
        clientStanzaId: String,
        expected: OutboundOwnership.NativeOwned,
    ): Boolean {
        var removed = false
        withContext(NonCancellable) {
            sessionPrefs.updateOutboundQueue { current ->
                current.filterNot { row ->
                    val matches = row.clientStanzaId == clientStanzaId && row.ownership == expected
                    if (matches) removed = true
                    matches
                }
            }
        }
        return removed
    }

    enum class FailureResolution {
        STALE,
        TRANSFERRED_TO_FALLBACK,
        RELEASED,
    }

    companion object {
        /**
         * Drop-oldest bound on the persisted queue: high enough for any
         * realistic offline burst, low enough to keep the DataStore blob
         * (and the replay storm on reconnect) small.
         */
        const val DEFAULT_CAPACITY = 50
    }
}
