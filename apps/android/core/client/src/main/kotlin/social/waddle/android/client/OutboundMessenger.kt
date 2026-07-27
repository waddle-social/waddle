package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
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
    private val sendMutex = Mutex()

    /**
     * One manager-level send. The typed semantic intent is persisted
     * before the FFI call, and one generated UUID is used as both
     * message `id` and XEP-0359 `origin-id`. A transport-accepted send
     * stays durable until its matching XEP-0198 acknowledgement.
     */
    suspend fun sendOrEnqueue(
        conversationJid: String,
        isGroupchat: Boolean,
        body: String,
        extras: MessageSendExtras? = null,
    ): SendResult = sendMutex.withLock {
        val clientStanzaId = newClientStanzaId()
        val owner = activeSession.ownBareJid
            ?: runCatching { sessionPrefs.ownerBareJid.first() }.getOrNull()
            ?: return@withLock SendResult(WaddleSendMessageOutcome.Error)
        val queued = QueuedOutboundMessage(
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
            markup = extras?.markup.orEmpty(),
            sticker = extras?.sticker,
        )
        val persisted = try {
            outboundQueue.enqueue(queued)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            return@withLock SendResult(WaddleSendMessageOutcome.Error)
        }
        if (persisted == OutboundQueue.EnqueueResult.FULL) {
            return@withLock SendResult(WaddleSendMessageOutcome.Error)
        }

        val outcome = sendMessage(conversationJid, isGroupchat, body, clientStanzaId, extras)
        when (outcome) {
            is WaddleSendMessageOutcome.Sent,
            WaddleSendMessageOutcome.NotConnected,
            WaddleSendMessageOutcome.TransportError,
            -> SendResult(outcome, queuedId = clientStanzaId)
            else -> {
                outboundQueue.remove(owner, clientStanzaId)
                reportDroppedQueuedMessage(queued, outcome::class.simpleName ?: DROP_REASON_UNKNOWN)
                SendResult(outcome)
            }
        }
    }

    /**
     * Replays the persisted outbound queue through the live attempt's
     * client once per retained row on every fresh `SessionReady`.
     * Transport acceptance is deliberately at-least-once until the
     * matching XEP-0198 acknowledgement removes the durable intent;
     * server-side effect deduplication is the following P1 concern.
     */
    suspend fun drainOutboundQueue() {
        sendMutex.withLock {
            val owner = activeSession.ownBareJid ?: return@withLock
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
    }

    /** Persisted delivery ownership moves to the server before UI dispatch. */
    suspend fun acknowledgeDelivery(clientStanzaId: String) {
        val owner = activeSession.ownBareJid ?: return
        outboundQueue.acknowledge(owner, clientStanzaId)
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
        return if (outcome is WaddleSendMessageOutcome.Sent) {
            WaddleSendMessageOutcome.Sent(stanzaId)
        } else {
            outcome
        }
    }

    /**
     * A queued message will never be delivered (a permanent synchronous
     * rejection): `DeliveryFailed` flips any optimistic
     * row that tracks the id to the retryable failed state — factual,
     * not a faked ack — and the `Error` diagnostic surfaces the drop
     * even when no conversation screen is tracking it.
     */
    private fun reportDroppedQueuedMessage(message: QueuedOutboundMessage, reason: String) {
        dispatchEvent(XmppEvent.DeliveryFailed(message.clientStanzaId))
        dispatchEvent(XmppEvent.Error("dropped queued message to ${message.conversationJid}: $reason"))
    }

    private companion object {
        const val DROP_REASON_UNKNOWN = "rejected"
    }
}
