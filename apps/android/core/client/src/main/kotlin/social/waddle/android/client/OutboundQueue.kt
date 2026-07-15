package social.waddle.android.client

import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.withContext
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.client.ffi.WaddleSendMessageOutcome

/**
 * Persisted outbound send queue (web `waddle.chat.outbound-queue`
 * parity): sends that failed because no live session existed are kept in
 * [SessionPrefs] and replayed, in enqueue order, on the next
 * `SessionReady` — so a message typed while offline survives process
 * death instead of being lost.
 */
class OutboundQueue(
    private val sessionPrefs: SessionPrefs,
    private val capacity: Int = DEFAULT_CAPACITY,
) {
    /**
     * Persist [message]; a replay of an id already in the queue replaces
     * the old entry. At [capacity] the OLDEST entry is evicted and
     * returned so the caller can surface the drop.
     */
    suspend fun enqueue(message: QueuedOutboundMessage): QueuedOutboundMessage? {
        var evicted: QueuedOutboundMessage? = null
        sessionPrefs.updateOutboundQueue { current ->
            val withoutReplaced = current.filterNot { it.clientStanzaId == message.clientStanzaId }
            if (withoutReplaced.size >= capacity) {
                evicted = withoutReplaced.first()
                withoutReplaced.drop(1) + message
            } else {
                withoutReplaced + message
            }
        }
        return evicted
    }

    /**
     * Replay the queue head-first through [send]. Per-message outcomes:
     * - `Sent` → remove (the real MUC echo / XEP-0198 ack flows
     *   normally; nothing is faked here).
     * - `NotConnected` / `TransportError` → keep and STOP: the session
     *   is gone again, the remainder retries on the next `SessionReady`.
     * - anything else (`InvalidRecipient`, `InvalidOptions`,
     *   `StanzaError`, `Error`) → drop and report via [onDropped]: the
     *   session was live and rejected this exact payload, so replaying
     *   it can only fail the same way forever.
     *
     * No lock is held across [send] (an FFI call): each iteration
     * re-reads the persisted head, so concurrent enqueues interleave
     * safely. Removal after a successful send is non-cancellable to
     * shrink the send-then-cancelled window in which a replay could
     * double-send (the id-stable replay is collapsed by timeline dedupe
     * anyway).
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
        while (true) {
            val head = sessionPrefs.outboundQueue.first().firstOrNull() ?: return
            when (val outcome = send(head)) {
                is WaddleSendMessageOutcome.Sent -> remove(head.clientStanzaId)
                WaddleSendMessageOutcome.NotConnected,
                WaddleSendMessageOutcome.TransportError,
                -> return
                else -> {
                    remove(head.clientStanzaId)
                    onDropped(head, outcome)
                }
            }
        }
    }

    private suspend fun pruneForeign(ownerBareJid: String) {
        withContext(NonCancellable) {
            sessionPrefs.updateOutboundQueue { current ->
                current.filter { it.ownerBareJid == ownerBareJid }
            }
        }
    }

    private suspend fun remove(clientStanzaId: String) {
        withContext(NonCancellable) {
            sessionPrefs.updateOutboundQueue { current ->
                current.filterNot { it.clientStanzaId == clientStanzaId }
            }
        }
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
