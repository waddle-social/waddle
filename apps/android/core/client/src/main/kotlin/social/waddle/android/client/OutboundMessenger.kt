package social.waddle.android.client

import kotlinx.coroutines.CancellationException
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
        val lease = activeSession.captureOwnerLease()
            ?: return@withLock SendResult(WaddleSendMessageOutcome.Error)
        val owner = lease.ownerBareJid
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
        // Validate the captured account attempt immediately before writing
        // durable state. A generation is required because a same-account
        // relogin would otherwise pass a bare-JID-only check.
        if (!activeSession.isCurrent(lease)) return@withLock SendResult(WaddleSendMessageOutcome.Error)
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

        // A logout/relogin can race the DataStore suspend point. Remove only
        // the row this stale attempt created; never touch a later owner's
        // durable queue, and never invoke its transport.
        if (!activeSession.isCurrent(lease)) {
            outboundQueue.remove(owner, clientStanzaId)
            return@withLock SendResult(WaddleSendMessageOutcome.Error)
        }

        // sendMessage validates the lease immediately before transport
        // selection and explicitly reports a stale no-transport outcome.
        val attempt = sendMessage(lease, conversationJid, isGroupchat, body, clientStanzaId, extras)
        if (attempt is ActiveSession.LeaseSendResult.Stale) {
            outboundQueue.remove(owner, clientStanzaId)
            return@withLock SendResult(WaddleSendMessageOutcome.Error)
        }
        val outcome = (attempt as ActiveSession.LeaseSendResult.Attempted).outcome
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
            val lease = activeSession.captureOwnerLease() ?: return@withLock
            val owner = lease.ownerBareJid
            // Keep OutboundQueue.drain's snapshot membership recheck and its
            // send callback under sendMutex. acknowledgeDelivery uses this
            // same mutex, making the exact durable removal and transport
            // selection one ordered decision.
            outboundQueue.drain(
                ownerBareJid = owner,
                send = { queued ->
                    if (!activeSession.isCurrent(lease)) {
                        outboundQueue.remove(owner, queued.clientStanzaId)
                        return@drain WaddleSendMessageOutcome.NotConnected
                    }
                    val attempt = sendMessage(
                        lease = lease,
                        conversationJid = queued.conversationJid,
                        isGroupchat = queued.isGroupchat,
                        body = queued.body,
                        stanzaId = queued.clientStanzaId,
                        extras = queued.sendExtras(),
                    )
                    if (attempt is ActiveSession.LeaseSendResult.Stale) {
                        outboundQueue.remove(owner, queued.clientStanzaId)
                        return@drain WaddleSendMessageOutcome.NotConnected
                    }
                    val outcome = (attempt as ActiveSession.LeaseSendResult.Attempted).outcome
                    if (!activeSession.isCurrent(lease)) {
                        outboundQueue.remove(owner, queued.clientStanzaId)
                        WaddleSendMessageOutcome.NotConnected
                    } else {
                        outcome
                    }
                },
                onDropped = { queued, outcome ->
                    reportDroppedQueuedMessage(queued, outcome::class.simpleName ?: DROP_REASON_UNKNOWN)
                },
            )
        }
    }

    /** Persisted delivery ownership moves to the server before UI dispatch. */
    suspend fun acknowledgeDelivery(clientStanzaId: String) {
        // The durable membership check in drainOutboundQueue and the FFI send
        // share this authority.  An acknowledgement that acquires it first
        // removes the exact intent before replay can select a client; one
        // that arrives after replay has acquired it observes the resulting
        // at-least-once send and removes the row afterwards.  Do not put a
        // DataStore transaction around transport I/O: sendMutex is the only
        // cross-suspension serialization point.
        sendMutex.withLock {
            val owner = activeSession.ownBareJid ?: return@withLock
            outboundQueue.acknowledge(owner, clientStanzaId)
        }
    }

    private suspend fun sendMessage(
        lease: ActiveSession.OwnerLease,
        conversationJid: String,
        isGroupchat: Boolean,
        body: String,
        stanzaId: String,
        extras: MessageSendExtras? = null,
    ): ActiveSession.LeaseSendResult {
        val (finalBody, options) = preparedSend(stanzaId, body, extras)
        val attempt = activeSession.sendIfCurrent(lease) { client ->
            if (isGroupchat) {
                client.sendGroupchatMessage(conversationJid, finalBody, options)
            } else {
                client.sendChatMessage(conversationJid, finalBody, options)
            }
        }
        if (attempt is ActiveSession.LeaseSendResult.Stale) return attempt
        val outcome = (attempt as ActiveSession.LeaseSendResult.Attempted).outcome
        // A DM send has no reflection: insert the local echo so peer
        // mutations (reactions, markers) can resolve their target and
        // the sender can edit/retract the fresh message (see ownDmEcho).
        if (!isGroupchat && outcome is WaddleSendMessageOutcome.Sent) {
            lease.takeIf(activeSession::isCurrent)?.ownerBareJid?.let { own ->
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
            ActiveSession.LeaseSendResult.Attempted(WaddleSendMessageOutcome.Sent(stanzaId))
        } else {
            ActiveSession.LeaseSendResult.Attempted(outcome)
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
