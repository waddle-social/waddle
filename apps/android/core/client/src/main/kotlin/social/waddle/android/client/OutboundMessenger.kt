package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.first
import social.waddle.android.client.prefs.NativeOutboundPhase
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.store.SessionStores
import social.waddle.client.ffi.WaddleResumeStanzaKind
import social.waddle.client.ffi.WaddleResumeXmlToken
import social.waddle.client.ffi.WaddleSendMessageOutcome
import social.waddle.client.ffi.WaddleSmResumeState

/**
 * Outbound message sends plus the durable journal: [sendOrEnqueue] is the
 * single manager-level send, and [drainOutboundQueue] replays eligible
 * rows on `SessionReady`.
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
     * XEP-0359 origin-id. Every send is first journaled for one exact
     * connection generation. `NotConnected` and `TransportError` release
     * the row for replay; a possibly-written transport error remains
     * uncertain until SM-state/generation reconciliation. Other failures
     * discard the row because replaying the rejected payload cannot help.
     */
    suspend fun sendOrEnqueue(
        conversationJid: String,
        isGroupchat: Boolean,
        body: String,
        extras: MessageSendExtras? = null,
    ): SendResult {
        val owner = activeSession.ownBareJid
            ?: runCatching { sessionPrefs.ownerBareJid.first() }.getOrNull()
            ?: return SendResult(WaddleSendMessageOutcome.NotConnected)
        val queued = queuedMessage(owner, conversationJid, isGroupchat, body, extras)
        val connectionGeneration = activeSession.connectionGeneration
        val ownership = OutboundOwnership.NativeOwned(
            connectionGeneration = connectionGeneration,
            phase = NativeOutboundPhase.FRESH,
        )
        val enqueue = persistQueueMutation {
            outboundQueue.enqueueClaimed(queued, ownership)
        } ?: return SendResult(WaddleSendMessageOutcome.Error)
        if (!enqueue.stored) {
            return SendResult(WaddleSendMessageOutcome.Error)
        }
        enqueue.evicted?.let { reportDroppedQueuedMessage(it, DROP_REASON_QUEUE_FULL) }

        val outcome = sendMessage(queued.copy(ownership = ownership), connectionGeneration)
        return reconcileInitialOutcome(queued.clientStanzaId, ownership, outcome)
    }

    private fun queuedMessage(
        owner: String,
        conversationJid: String,
        isGroupchat: Boolean,
        body: String,
        extras: MessageSendExtras?,
    ): QueuedOutboundMessage = QueuedOutboundMessage(
        ownerBareJid = owner,
        conversationJid = conversationJid,
        isGroupchat = isGroupchat,
        body = body,
        clientStanzaId = newClientStanzaId(),
        enqueuedAtMillis = System.currentTimeMillis(),
        replyToId = extras?.replyToId,
        replyToAuthorJid = extras?.replyToAuthorJid,
        replyParentBody = extras?.replyParentBody,
        threadId = extras?.threadId,
        threadParent = extras?.threadParent,
        sharedFiles = extras?.sharedFiles.orEmpty(),
        mentions = extras?.mentions.orEmpty(),
    )

    private suspend fun reconcileInitialOutcome(
        stanzaId: String,
        ownership: OutboundOwnership.NativeOwned,
        outcome: WaddleSendMessageOutcome,
    ): SendResult {
        if (outcome is WaddleSendMessageOutcome.Sent) {
            return SendResult(outcome)
        }
        val queueable = isQueueableFailure(outcome)
        val reconciled = persistQueueMutation {
            if (queueable) {
                outboundQueue.release(stanzaId, ownership)
            } else {
                outboundQueue.removeOwned(stanzaId, ownership)
            }
        }
        if (reconciled != true) {
            return SendResult(WaddleSendMessageOutcome.Error)
        }
        return if (queueable) {
            SendResult(outcome, queuedId = stanzaId)
        } else {
            SendResult(outcome)
        }
    }

    private suspend fun <T> persistQueueMutation(block: suspend () -> T): T? = try {
        block()
    } catch (cancellation: CancellationException) {
        throw cancellation
    } catch (_: Throwable) {
        null
    }

    /**
     * Replays only Ready rows through the live attempt. NativeOwned rows
     * remain fenced to their exact generation while XEP-0198 resume or
     * fresh fallback owns them. Without SM, handling is unknowable, so a
     * reconnect may release and replay the same stable XEP-0359 identity.
     */
    suspend fun drainOutboundQueue() {
        val owner = activeSession.ownBareJid ?: return
        val connectionGeneration = activeSession.connectionGeneration
        outboundQueue.drain(
            ownerBareJid = owner,
            connectionGeneration = connectionGeneration,
            send = { queued ->
                sendMessage(queued, connectionGeneration)
            },
            onDropped = { queued, outcome ->
                reportDroppedQueuedMessage(queued, outcome::class.simpleName ?: DROP_REASON_UNKNOWN)
            },
        )
    }

    suspend fun reconcileAttempt(state: WaddleSmResumeState?, connectionGeneration: Long) {
        val owner = activeSession.ownBareJid ?: return
        val resumeIds = state?.queuedEntries
            .orEmpty()
            .mapNotNull { entry ->
                if (entry.stanza.stanzaKind != WaddleResumeStanzaKind.MESSAGE) return@mapNotNull null
                val root = entry.stanza.tokens.firstOrNull() as? WaddleResumeXmlToken.Start
                    ?: return@mapNotNull null
                if (
                    root.name.namespace.value != "jabber:client" ||
                    root.name.localName.value != "message"
                ) {
                    return@mapNotNull null
                }
                root.attributes.firstOrNull { attribute ->
                    attribute.name.namespace.value.isEmpty() &&
                        attribute.name.localName.value == "id"
                }?.value?.value
            }
            .toSet()
        outboundQueue.reconcileAttempt(owner, connectionGeneration, resumeIds)
    }

    /** Commit exact-generation durable ownership before the event reaches
     * stores/UI. Returns false for stale events and the first resume failure,
     * which transfers native ownership to fresh-stream fallback. */
    suspend fun reconcileDeliveryEvent(event: XmppEvent, connectionGeneration: Long): Boolean =
        when (event) {
            is XmppEvent.DeliveryAcked ->
                outboundQueue.acknowledge(event.stanzaId, connectionGeneration)
            is XmppEvent.DeliveryFailed ->
                when (outboundQueue.failNative(event.stanzaId, connectionGeneration)) {
                    OutboundQueue.FailureResolution.RELEASED -> true
                    OutboundQueue.FailureResolution.STALE,
                    OutboundQueue.FailureResolution.TRANSFERRED_TO_FALLBACK,
                    -> false
                }
            else -> true
        }

    private suspend fun sendMessage(
        queued: QueuedOutboundMessage,
        connectionGeneration: Long,
    ): WaddleSendMessageOutcome {
        val conversationJid = queued.conversationJid
        val isGroupchat = queued.isGroupchat
        val body = queued.body
        val stanzaId = queued.clientStanzaId
        val extras = queued.sendExtras()
        val (finalBody, options) = preparedSend(stanzaId, body, extras)
        val outcome = activeSession.sendAtGeneration(connectionGeneration) { client ->
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
