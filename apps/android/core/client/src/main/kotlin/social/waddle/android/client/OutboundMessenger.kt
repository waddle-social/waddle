package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import social.waddle.android.client.OutboundQueue.EnqueueResult
import social.waddle.android.client.OutboundQueue.ResumeTransitionResult
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryAttemptTransition
import social.waddle.android.client.prefs.DeliverySource
import social.waddle.android.client.prefs.DeliveryTerminalKind
import social.waddle.android.client.prefs.NativeOutboundPhase
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.QueuedOutboundDraft
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.session.ResumePersistence
import social.waddle.android.client.store.SessionStores
import social.waddle.client.ffi.WaddleSendMessageOutcome
import java.util.logging.Level
import java.util.logging.Logger

/**
 * Outbound message sends plus the owner-scoped durable journal.
 *
 * A row is committed and exactly claimed before FFI. Native terminal signals
 * are queued to [DeliveryTerminalWorker] and never perform DataStore I/O on
 * ConnectionLoop's socket consumer.
 */
internal class OutboundMessenger(
    private val activeSession: ActiveSession,
    private val stores: SessionStores,
    private val sessionPrefs: SessionPrefs,
    private val journal: OutboundQueue,
    private val resume: ResumePersistence,
    private val dispatchEvent: (XmppEvent) -> Unit,
) {
    private val terminalWorker =
        DeliveryTerminalWorker(journal, dispatchEvent)

    fun start(scope: CoroutineScope, ownerBareJid: String) {
        terminalWorker.start(scope, ownerBareJid)
    }

    suspend fun prepareAttempt(ownerBareJid: String): OutboundQueue.AttemptBootstrap =
        journal.beginAttempt(ownerBareJid).also { prepared ->
            resume.registerAttempt(prepared.attempt, prepared.smVersion)
        }

    fun retireAttempt(attempt: DeliveryAttemptRef) {
        resume.retireAttempt(attempt)
    }

    suspend fun awaitStartupTerminalDrain(ownerBareJid: String) {
        terminalWorker.awaitStartupDrain(ownerBareJid)
    }

    suspend fun fenceAndStop(
        attempt: DeliveryAttemptRef?,
    ): DeliveryTerminalWorker.StopResult {
        val owner = attempt?.ownerBareJid
            ?: activeSession.ownBareJid
            ?: return DeliveryTerminalWorker.StopResult.Drained
        // Close worker admissions and durably drain already-admitted signals
        // while their exact attempt fence is still valid. Only then revoke
        // the attempt so no later callback can mutate the journal.
        val result = terminalWorker.stop(owner)
        if (attempt != null) journal.fenceAttempt(attempt)
        return result
    }

    /**
     * One manager-level send. [expectedOwnerBareJid] is required by
     * process-death entry points (notification direct reply) so a stale
     * account-A intent can never enqueue under account B.
     */
    suspend fun sendOrEnqueue(
        conversationJid: String,
        isGroupchat: Boolean,
        body: String,
        extras: MessageSendExtras? = null,
        expectedOwnerBareJid: String? = null,
        source: DeliverySource = DeliverySource.Composer,
    ): SendResult {
        val owner = activeSession.ownBareJid
            ?: runCatching { sessionPrefs.ownerBareJid.first() }.getOrNull()
            ?: return SendResult(WaddleSendMessageOutcome.NotConnected)
        if (expectedOwnerBareJid != null && expectedOwnerBareJid != owner) {
            return SendResult(WaddleSendMessageOutcome.Error)
        }
        val draft = queuedMessage(owner, conversationJid, isGroupchat, body, extras, source)
        val attempt = activeSession.attemptRef
        val enqueue = persistQueueMutation {
            if (attempt == null) {
                journal.enqueueReady(draft)
            } else {
                journal.enqueueClaimed(draft, attempt)
            }
        } ?: return SendResult(WaddleSendMessageOutcome.Error)
        val stored = when (enqueue) {
            is EnqueueResult.Stored -> enqueue.row
            is EnqueueResult.Conflict,
            EnqueueResult.CapacityExhausted,
            EnqueueResult.StaleAttempt,
            -> return SendResult(WaddleSendMessageOutcome.Error)
        }
        val delivery = DeliveryOutcomeRef(stored.identity, stored.source)
        if (attempt == null) {
            return SendResult(
                outcome = WaddleSendMessageOutcome.NotConnected,
                delivery = delivery,
            )
        }

        val ownership = stored.ownership as? OutboundOwnership.NativeOwned
            ?: return SendResult(WaddleSendMessageOutcome.Error)
        val outcome = sendMessage(stored, attempt)
        return reconcileInitialOutcome(stored, ownership, outcome)
    }

    private fun queuedMessage(
        owner: String,
        conversationJid: String,
        isGroupchat: Boolean,
        body: String,
        extras: MessageSendExtras?,
        source: DeliverySource,
    ): QueuedOutboundDraft = QueuedOutboundDraft.create(
        ownerBareJid = owner,
        conversationJid = conversationJid,
        isGroupchat = isGroupchat,
        body = body,
        clientStanzaId = newClientStanzaId(),
        enqueuedAtMillis = System.currentTimeMillis(),
        source = source,
        replyToId = extras?.replyToId,
        replyToAuthorJid = extras?.replyToAuthorJid,
        replyParentBody = extras?.replyParentBody,
        threadId = extras?.threadId,
        threadParent = extras?.threadParent,
        sharedFiles = extras?.sharedFiles.orEmpty(),
        mentions = extras?.mentions.orEmpty(),
    )

    private suspend fun reconcileInitialOutcome(
        row: QueuedOutboundMessage,
        ownership: OutboundOwnership.NativeOwned,
        outcome: WaddleSendMessageOutcome,
    ): SendResult {
        val delivery = DeliveryOutcomeRef(row.identity, row.source)
        if (
            outcome is WaddleSendMessageOutcome.Sent &&
            outcome.stanzaId == row.clientStanzaId
        ) {
            return SendResult(outcome, delivery)
        }
        if (isQueueableFailure(outcome)) {
            val released = persistQueueMutation {
                journal.release(row.identity, ownership)
            }
            return if (released == true) {
                SendResult(outcome, delivery)
            } else {
                SendResult(WaddleSendMessageOutcome.Error)
            }
        }

        terminalWorker.submitAndAwait(
            ownerBareJid = row.ownerBareJid,
            clientStanzaId = row.clientStanzaId,
            attempt = ownership.attempt,
            kind = DeliveryTerminalKind.NONRETRYABLE_DELETE,
        )
        drainOutboundQueue()
        return SendResult(
            if (outcome is WaddleSendMessageOutcome.Sent) {
                WaddleSendMessageOutcome.Error
            } else {
                outcome
            },
        )
    }

    private suspend fun <T> persistQueueMutation(block: suspend () -> T): T? = try {
        block()
    } catch (cancellation: CancellationException) {
        throw cancellation
    } catch (_: Throwable) {
        null
    }

    /**
     * Replay owner Ready rows after the startup terminal barrier. Rows already
     * native-owned or terminal are never selected.
     */
    suspend fun drainOutboundQueue() {
        val owner = activeSession.ownBareJid ?: return
        awaitStartupTerminalDrain(owner)
        while (true) {
            val attempt = activeSession.attemptRef ?: return
            val ready = journal.readyHead(owner) ?: return
            val claimed = journal.claimReady(ready.identity, attempt) ?: continue
            val ownership = claimed.ownership as OutboundOwnership.NativeOwned
            when (val outcome = sendMessage(claimed, attempt)) {
                is WaddleSendMessageOutcome.Sent -> {
                    if (outcome.stanzaId != claimed.clientStanzaId) {
                        terminalWorker.submitAndAwait(
                            owner,
                            claimed.clientStanzaId,
                            ownership.attempt,
                            DeliveryTerminalKind.NONRETRYABLE_DELETE,
                        )
                    }
                }
                WaddleSendMessageOutcome.NotConnected,
                WaddleSendMessageOutcome.TransportError,
                -> {
                    journal.release(claimed.identity, ownership)
                    return
                }
                else -> {
                    terminalWorker.submitAndAwait(
                        owner,
                        claimed.clientStanzaId,
                        ownership.attempt,
                        DeliveryTerminalKind.NONRETRYABLE_DELETE,
                    )
                }
            }
        }
    }

    /**
     * Commit the exact Rust-minted RESUME -> FRESH_FALLBACK transition
     * before the connection loop polls another native event.
     */
    suspend fun rotateAndAwait(
        transition: DeliveryAttemptTransition,
        affectedStanzaIds: Set<String>,
    ): Boolean {
        val result = retryResumeTransition(transition, affectedStanzaIds)
        val smVersion = when (result) {
            is ResumeTransitionResult.Committed -> result.smVersion
            is ResumeTransitionResult.AlreadyCommitted -> result.smVersion
            is ResumeTransitionResult.AffectedSetMismatch,
            ResumeTransitionResult.InvalidTransition,
            ResumeTransitionResult.ReceiptCapacityExhausted,
            ResumeTransitionResult.ReceiptConflict,
            ResumeTransitionResult.StaleAttempt,
            -> return false
        }
        if (!activeSession.acceptResumeTransition(transition)) return false
        return resume.acceptResumeTransition(transition, smVersion)
    }

    private suspend fun retryResumeTransition(
        transition: DeliveryAttemptTransition,
        affectedStanzaIds: Set<String>,
    ): ResumeTransitionResult {
        var retryIndex = 0
        while (true) {
            try {
                return journal.rotateAfterResumeFailure(transition, affectedStanzaIds)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (failure: Throwable) {
                LOGGER.log(Level.WARNING, "resume transition commit failed; retrying", failure)
                delay(RETRY_DELAYS_MILLIS[retryIndex.coerceAtMost(RETRY_DELAYS_MILLIS.lastIndex)])
                if (retryIndex < RETRY_DELAYS_MILLIS.lastIndex) retryIndex += 1
            }
        }
    }

    /** Apply native terminal signals before asking Rust for another event. */
    suspend fun reconcileDeliveryEvent(event: XmppEvent): Boolean =
        when (event) {
            is XmppEvent.NativeDeliveryAcked -> {
                terminalWorker.submitAndAwait(
                    event.attempt.ownerBareJid,
                    event.clientStanzaId,
                    event.attempt,
                    DeliveryTerminalKind.ACK,
                )
                drainOutboundQueue()
                false
            }
            is XmppEvent.NativeDeliveryFailed -> {
                terminalWorker.submitAndAwait(
                    event.attempt.ownerBareJid,
                    event.clientStanzaId,
                    event.attempt,
                    DeliveryTerminalKind.NATIVE_FAILURE,
                )
                drainOutboundQueue()
                false
            }
            else -> true
        }

    private suspend fun sendMessage(
        queued: QueuedOutboundMessage,
        attempt: DeliveryAttemptRef,
    ): WaddleSendMessageOutcome {
        val (finalBody, options) = preparedSend(
            queued.clientStanzaId,
            queued.body,
            queued.sendExtras(),
        )
        val outcome = activeSession.sendAtAttempt(attempt) { client ->
            if (queued.isGroupchat) {
                client.sendGroupchatMessage(queued.conversationJid, finalBody, options)
            } else {
                client.sendChatMessage(queued.conversationJid, finalBody, options)
            }
        }
        if (!queued.isGroupchat && outcome is WaddleSendMessageOutcome.Sent) {
            activeSession.ownBareJid?.let { own ->
                stores.timelineStore.onLiveMessage(
                    ownDmEcho(
                        ownJid = own,
                        peerJid = queued.conversationJid,
                        stanzaId = queued.clientStanzaId,
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

    private companion object {
        val LOGGER: Logger = Logger.getLogger(OutboundMessenger::class.java.name)
        val RETRY_DELAYS_MILLIS = longArrayOf(250L, 500L, 1_000L, 2_000L, 5_000L)
    }
}
