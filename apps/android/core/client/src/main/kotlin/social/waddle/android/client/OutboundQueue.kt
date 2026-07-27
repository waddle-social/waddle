package social.waddle.android.client

import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.withContext
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.client.ffi.WaddleSendMessageOutcome

/**
 * Persisted outbound intent queue. Every chat/groupchat send enters this
 * queue before the FFI transport is called and remains until the matching
 * XEP-0198 acknowledgement transfers responsibility to the server.
 */
class OutboundQueue(
    private val sessionPrefs: SessionPrefs,
    private val capacity: Int = DEFAULT_CAPACITY,
) {
    /**
     * Atomically persist [message]. Accepted rows are never evicted: a new
     * row at [capacity] is rejected without changing the existing queue.
     * Re-inserting the same owned identity is idempotent.
     */
    suspend fun enqueue(message: QueuedOutboundMessage): EnqueueResult {
        var result = EnqueueResult.ACCEPTED
        sessionPrefs.updateOutboundQueue { current ->
            when {
                current.any { it.sameIdentityAs(message) } -> current
                current.size >= capacity -> {
                    result = EnqueueResult.FULL
                    current
                }
                else -> current + message
            }
        }
        return result
    }

    /**
     * Replay one queue snapshot head-first through [send]. Per-message outcomes:
     * - `Sent` → keep until the matching XEP-0198 acknowledgement.
     * - `NotConnected` / `TransportError` → keep and STOP: the session
     *   is gone again, the remainder retries on the next `SessionReady`.
     * - anything else (`InvalidRecipient`, `InvalidOptions`,
     *   `StanzaError`, `Error`) → remove and report via [onDropped]: the
     *   session was live and rejected this exact payload, so replaying
     *   it can only fail the same way forever.
     *
     * A snapshot means each retained row is attempted at most once per
     * fresh ready session. Before each send the durable row is rechecked,
     * so an acknowledgement racing the drain prevents a stale replay.
     */
    suspend fun drain(
        ownerBareJid: String,
        send: suspend (QueuedOutboundMessage) -> WaddleSendMessageOutcome,
        onDropped: suspend (QueuedOutboundMessage, WaddleSendMessageOutcome) -> Unit,
    ) {
        // Cross-account guard: drop anything another account enqueued
        // (e.g. a send committed in the logout teardown window) BEFORE
        // draining — replaying it here would misdeliver it under the
        // current account.
        pruneForeign(ownerBareJid)
        val snapshot = sessionPrefs.outboundQueue.first()
            .filter { it.ownerBareJid == ownerBareJid }
        for (message in snapshot) {
            if (!contains(ownerBareJid, message.clientStanzaId)) continue
            when (val outcome = send(message)) {
                is WaddleSendMessageOutcome.Sent -> Unit
                WaddleSendMessageOutcome.NotConnected,
                WaddleSendMessageOutcome.TransportError,
                -> return
                else -> {
                    remove(ownerBareJid, message.clientStanzaId)
                    onDropped(message, outcome)
                }
            }
        }
    }

    /** Delete the exact row owned by [ownerBareJid] after its SM ack. */
    suspend fun acknowledge(ownerBareJid: String, clientStanzaId: String) {
        remove(ownerBareJid, clientStanzaId)
    }

    private suspend fun pruneForeign(ownerBareJid: String) {
        withContext(NonCancellable) {
            sessionPrefs.updateOutboundQueue { current ->
                current.filter { it.ownerBareJid == ownerBareJid }
            }
        }
    }

    suspend fun remove(ownerBareJid: String, clientStanzaId: String) {
        withContext(NonCancellable) {
            sessionPrefs.updateOutboundQueue { current ->
                current.filterNot {
                    it.ownerBareJid == ownerBareJid && it.clientStanzaId == clientStanzaId
                }
            }
        }
    }

    private suspend fun contains(ownerBareJid: String, clientStanzaId: String): Boolean =
        sessionPrefs.outboundQueue.first().any {
            it.ownerBareJid == ownerBareJid && it.clientStanzaId == clientStanzaId
        }

    private fun QueuedOutboundMessage.sameIdentityAs(other: QueuedOutboundMessage): Boolean =
        ownerBareJid == other.ownerBareJid && clientStanzaId == other.clientStanzaId

    enum class EnqueueResult {
        ACCEPTED,
        FULL,
    }

    companion object {
        /**
         * Reject-new bound on the persisted queue: high enough for any
         * realistic offline burst, low enough to keep the DataStore blob
         * (and the replay storm on reconnect) small.
         */
        const val DEFAULT_CAPACITY = 50
    }
}
