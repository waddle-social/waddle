package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.first
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.store.SessionStores
import social.waddle.client.ffi.WaddleSendMessageOutcome

/**
 * Outbound message sends plus the persisted offline queue:
 * [sendOrEnqueue] is the single manager-level send, and
 * [drainOutboundQueue] replays the queue on `SessionReady`.
 */
internal class OutboundMessenger(
    private val activeSession: ActiveSession,
    private val stores: SessionStores,
    private val sessionPrefs: SessionPrefs,
    private val dispatchEvent: (XmppEvent) -> Unit,
) {
    private val outboundQueue = OutboundQueue(sessionPrefs)

    /**
     * One manager-level send: the client stanza id is generated HERE
     * (not by the FFI) so a queued replay can resend under the same
     * XEP-0359 origin-id. `NotConnected`/`TransportError` mean no live
     * session carried the message — those enqueue for replay on the
     * next `SessionReady` and hand the queue id back via
     * [SendResult.queuedId]; every other outcome passes through
     * untouched (a live session rejected the payload — replaying the
     * identical stanza cannot succeed).
     */
    suspend fun sendOrEnqueue(
        conversationJid: String,
        isGroupchat: Boolean,
        body: String,
        extras: MessageSendExtras? = null,
    ): SendResult {
        val clientStanzaId = newClientStanzaId()
        val outcome = sendMessage(conversationJid, isGroupchat, body, clientStanzaId, extras)
        if (!isQueueableFailure(outcome)) return SendResult(outcome)
        // A logout can race this persist (reply-receiver sends run on the
        // process scope): never enqueue without an owner, and the owned
        // entry gets pruned by the next account's drain if it survives
        // the teardown window. Process-death revivals (notification
        // direct replies) have no in-memory owner yet — fall back to the
        // persisted one so the reply queues instead of being discarded;
        // logout clears that key too, keeping the teardown race safe.
        val owner = activeSession.ownBareJid
            ?: runCatching { sessionPrefs.ownerBareJid.first() }.getOrNull()
            ?: return SendResult(outcome)
        val evicted = try {
            outboundQueue.enqueue(
                QueuedOutboundMessage(
                    ownerBareJid = owner,
                    conversationJid = conversationJid,
                    isGroupchat = isGroupchat,
                    body = body,
                    clientStanzaId = clientStanzaId,
                    enqueuedAtMillis = System.currentTimeMillis(),
                    replyToId = extras?.replyToId,
                    replyToAuthorJid = extras?.replyToAuthorJid,
                    replyParentBody = extras?.replyParentBody,
                    threadId = extras?.threadId,
                    threadParent = extras?.threadParent,
                    sharedFiles = extras?.sharedFiles.orEmpty(),
                    mentions = extras?.mentions.orEmpty(),
                ),
            )
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            // Persistence is best-effort: a failed enqueue write behaves
            // like the pre-queue behavior (the outcome already reports
            // the failure) instead of crashing the sender's scope.
            return SendResult(outcome)
        }
        evicted?.let { reportDroppedQueuedMessage(it, DROP_REASON_QUEUE_FULL) }
        return SendResult(outcome, queuedId = clientStanzaId)
    }

    /**
     * Replays the persisted outbound queue through the live attempt's
     * client. Runs unconditionally on every `SessionReady` (a resumed
     * stream replays 0198-unacked stanzas itself, but the persisted
     * queue only ever holds messages NO stream accepted, so replaying
     * them here can never duplicate a resume replay).
     */
    suspend fun drainOutboundQueue() {
        val owner = activeSession.ownBareJid ?: return
        outboundQueue.drain(
            ownerBareJid = owner,
            send = { queued ->
                sendMessage(
                    conversationJid = queued.conversationJid,
                    isGroupchat = queued.isGroupchat,
                    body = queued.body,
                    stanzaId = queued.clientStanzaId,
                    extras = queued.sendExtras(),
                )
            },
            onDropped = { queued, outcome ->
                reportDroppedQueuedMessage(queued, outcome::class.simpleName ?: DROP_REASON_UNKNOWN)
            },
        )
    }

    private suspend fun sendMessage(
        conversationJid: String,
        isGroupchat: Boolean,
        body: String,
        stanzaId: String,
        extras: MessageSendExtras? = null,
    ): WaddleSendMessageOutcome {
        val (finalBody, options) = preparedSend(stanzaId, body, extras)
        val outcome = activeSession.send { client ->
            if (isGroupchat) {
                client.sendGroupchatMessage(conversationJid, finalBody, options)
            } else {
                client.sendChatMessage(conversationJid, finalBody, options)
            }
        }
        // A DM send has no reflection: insert the local echo so peer
        // mutations (reactions, markers) can resolve their target and
        // the sender can edit/retract the fresh message (see ownDmEcho).
        if (!isGroupchat && outcome is WaddleSendMessageOutcome.Sent) {
            activeSession.ownBareJid?.let { own ->
                stores.timelineStore.onLiveMessage(
                    ownDmEcho(
                        ownJid = own,
                        peerJid = conversationJid,
                        stanzaId = stanzaId,
                        body = finalBody,
                        options = options,
                    ),
                )
            }
        }
        return outcome
    }

    private fun isQueueableFailure(outcome: WaddleSendMessageOutcome): Boolean =
        outcome == WaddleSendMessageOutcome.NotConnected ||
            outcome == WaddleSendMessageOutcome.TransportError

    /**
     * A queued message will never be delivered (cap eviction or a
     * permanent replay rejection): `DeliveryFailed` flips any optimistic
     * row that tracks the id to the retryable failed state — factual,
     * not a faked ack — and the `Error` diagnostic surfaces the drop
     * even when no conversation screen is tracking it.
     */
    private fun reportDroppedQueuedMessage(message: QueuedOutboundMessage, reason: String) {
        dispatchEvent(XmppEvent.DeliveryFailed(message.clientStanzaId))
        dispatchEvent(XmppEvent.Error("dropped queued message to ${message.conversationJid}: $reason"))
    }

    private companion object {
        const val DROP_REASON_QUEUE_FULL = "outbound queue full, oldest evicted"
        const val DROP_REASON_UNKNOWN = "rejected"
    }
}
