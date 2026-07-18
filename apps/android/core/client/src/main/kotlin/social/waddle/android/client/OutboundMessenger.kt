package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import social.waddle.android.client.OutboundQueue.EnqueueResult
import social.waddle.android.client.OutboundQueue.LiveAdmissionResult
import social.waddle.android.client.OutboundQueue.ResumeTransitionResult
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryAttemptTransition
import social.waddle.android.client.prefs.DeliverySource
import social.waddle.android.client.prefs.DeliveryTerminalKind
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.QueuedOutboundDraft
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.QueuedOutboundPayload
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
    private val drainMutex = Mutex()

    @Volatile
    private var drainWorker: DrainWorkerGeneration? = null

    fun start(scope: CoroutineScope, ownerBareJid: String) {
        check(drainWorker == null) { "outbound drain worker already started" }
        terminalWorker.start(scope, ownerBareJid)
        val generation = DrainWorkerGeneration(
            ownerBareJid = ownerBareJid,
            signals = Channel<DrainWakeSignal>(Channel.CONFLATED),
        )
        drainWorker = generation
        generation.job = scope.launch {
            for (signal in generation.signals) {
                try {
                    drainOutboundQueue(signal.ownerBareJid, signal.attempt)
                } catch (cancellation: CancellationException) {
                    throw cancellation
                } catch (failure: Throwable) {
                    LOGGER.log(
                        Level.WARNING,
                        "outbound predecessor drain failed; durable work remains queued",
                        failure,
                    )
                }
            }
        }
    }

    suspend fun prepareAttempt(ownerBareJid: String): OutboundQueue.AttemptBootstrap =
        journal.beginAttempt(ownerBareJid).also { prepared ->
            bindDrainAttempt(prepared.attempt)
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
        if (!stopDrainWorker(owner, attempt)) {
            if (attempt != null) journal.fenceAttempt(attempt)
            return DeliveryTerminalWorker.StopResult.Drained
        }
        // Cancel and join the exact owner/session drain worker before closing
        // terminal admissions. Only then revoke the attempt so no queued wake
        // or callback can cross the replacement boundary.
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
        val draft = queuedMessage(
            owner = owner,
            source = source,
            payload = queuedPayload(conversationJid, isGroupchat, body, extras),
        )
        val attempt = activeSession.attemptRef
        if (attempt == null) {
            val enqueue = persistQueueMutation {
                journal.enqueueReady(draft)
            } ?: return SendResult(WaddleSendMessageOutcome.Error)
            val stored = when (enqueue) {
                is EnqueueResult.Stored -> enqueue.row
                is EnqueueResult.Conflict,
                EnqueueResult.CapacityExhausted,
                EnqueueResult.StaleAttempt,
                -> return SendResult(WaddleSendMessageOutcome.Error)
            }
            return SendResult(
                outcome = WaddleSendMessageOutcome.NotConnected,
                delivery = DeliveryOutcomeRef(stored.identity, stored.source),
            )
        }

        val admission = persistQueueMutation {
            journal.enqueueAndClaimAbsoluteHead(draft, attempt)
        } ?: return SendResult(WaddleSendMessageOutcome.Error)
        return when (admission) {
            is LiveAdmissionResult.Claimed -> {
                val stored = admission.row
                val ownership = stored.ownership as? OutboundOwnership.NativeOwned
                    ?: return SendResult(WaddleSendMessageOutcome.Error)
                val outcome = sendMessage(stored, attempt)
                reconcileInitialOutcome(stored, ownership, outcome)
            }
            is LiveAdmissionResult.Queued -> {
                signalOutboundDrain(attempt)
                SendResult(
                    outcome = WaddleSendMessageOutcome.NotConnected,
                    delivery = DeliveryOutcomeRef(
                        admission.row.identity,
                        admission.row.source,
                    ),
                )
            }
            is LiveAdmissionResult.Conflict,
            LiveAdmissionResult.CapacityExhausted,
            LiveAdmissionResult.StaleAttempt,
            -> SendResult(WaddleSendMessageOutcome.Error)
        }
    }

    private fun queuedMessage(
        owner: String,
        source: DeliverySource,
        payload: QueuedOutboundPayload,
    ): QueuedOutboundDraft = QueuedOutboundDraft.create(
        ownerBareJid = owner,
        clientStanzaId = newClientStanzaId(),
        enqueuedAtMillis = System.currentTimeMillis(),
        payload = payload,
        source = source,
    )

    private fun queuedPayload(
        conversationJid: String,
        isGroupchat: Boolean,
        body: String,
        extras: MessageSendExtras?,
    ): QueuedOutboundPayload = QueuedOutboundPayload(
        conversationJid = conversationJid,
        isGroupchat = isGroupchat,
        body = body,
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

    private fun signalOutboundDrain(attempt: DeliveryAttemptRef) {
        val generation = drainWorker
        if (
            generation?.ownerBareJid != attempt.ownerBareJid ||
            generation.attempt != attempt
        ) {
            return
        }
        generation.signals.trySend(
            DrainWakeSignal(attempt.ownerBareJid, attempt),
        )
    }

    private fun bindDrainAttempt(attempt: DeliveryAttemptRef) {
        val generation = drainWorker
        if (generation?.ownerBareJid == attempt.ownerBareJid) {
            generation.attempt = attempt
        }
    }

    private suspend fun stopDrainWorker(
        ownerBareJid: String,
        attempt: DeliveryAttemptRef?,
    ): Boolean {
        val generation = drainWorker ?: return true
        if (generation.ownerBareJid != ownerBareJid) return false
        if (attempt != null && generation.attempt != attempt) return false
        generation.signals.close()
        generation.job?.cancelAndJoin()
        if (drainWorker === generation) drainWorker = null
        return true
    }

    /**
     * Replay owner Ready rows after the startup terminal barrier. Rows already
     * native-owned or terminal are never selected.
     */
    suspend fun drainOutboundQueue() {
        val owner = activeSession.ownBareJid ?: return
        val attempt = activeSession.attemptRef ?: return
        drainOutboundQueue(owner, attempt)
    }

    private suspend fun drainOutboundQueue(
        ownerBareJid: String,
        expectedAttempt: DeliveryAttemptRef,
    ) = drainMutex.withLock {
        if (
            activeSession.ownBareJid != ownerBareJid ||
            activeSession.attemptRef != expectedAttempt
        ) {
            return@withLock
        }
        awaitStartupTerminalDrain(ownerBareJid)
        while (
            activeSession.ownBareJid == ownerBareJid &&
            activeSession.attemptRef == expectedAttempt
        ) {
            val attempt = activeSession.attemptRef ?: return@withLock
            val claimed =
                journal.claimAbsoluteReadyHead(ownerBareJid, attempt)
                    ?: return@withLock
            val ownership = claimed.ownership as OutboundOwnership.NativeOwned
            when (val outcome = sendMessage(claimed, attempt)) {
                is WaddleSendMessageOutcome.Sent -> {
                    if (outcome.stanzaId != claimed.clientStanzaId) {
                        terminalWorker.submitAndAwait(
                            ownerBareJid,
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
                    return@withLock
                }
                else -> {
                    terminalWorker.submitAndAwait(
                        ownerBareJid,
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
        if (!resume.acceptResumeTransition(transition, smVersion)) return false
        bindDrainAttempt(transition.fresh)
        return true
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

    private data class DrainWakeSignal(
        val ownerBareJid: String,
        val attempt: DeliveryAttemptRef,
    )

    private class DrainWorkerGeneration(
        val ownerBareJid: String,
        val signals: Channel<DrainWakeSignal>,
    ) {
        @Volatile
        var attempt: DeliveryAttemptRef? = null

        @Volatile
        var job: Job? = null
    }

    private companion object {
        val LOGGER: Logger = Logger.getLogger(OutboundMessenger::class.java.name)
        val RETRY_DELAYS_MILLIS = longArrayOf(250L, 500L, 1_000L, 2_000L, 5_000L)
    }
}
