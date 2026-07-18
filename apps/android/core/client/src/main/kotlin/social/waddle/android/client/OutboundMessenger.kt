package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import social.waddle.android.client.OutboundQueue.EnqueueResult
import social.waddle.android.client.OutboundQueue.LiveAdmissionResult
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryAttemptTransition
import social.waddle.android.client.prefs.DeliverySource
import social.waddle.android.client.prefs.DeliveryTerminalKind
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.QueuedOutboundContent
import social.waddle.android.client.prefs.QueuedOutboundDraft
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.QueuedOutboundPayload
import social.waddle.android.client.prefs.QueuedOutboundReply
import social.waddle.android.client.prefs.QueuedOutboundTarget
import social.waddle.android.client.prefs.QueuedOutboundThread
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.session.ResumePersistence
import social.waddle.android.client.store.SessionStores
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleSendMessageOutcome

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
    private val journal: OutboundQueue,
    private val resume: ResumePersistence,
    private val dispatchEvent: (XmppEvent) -> Unit,
    transitionTimeoutMillis: Long = 5_000L,
    phaseObserver: OutboundLifecyclePhaseObserver =
        OutboundLifecyclePhaseObserver.NONE,
) {
    private val drainMutex = Mutex()
    private val lifecycle = OutboundLifecycleCoordinator(
        activeSession = activeSession,
        journal = journal,
        resume = resume,
        dispatchEvent = dispatchEvent,
        drain = ::drainOutboundQueue,
        transitionTimeoutMillis = transitionTimeoutMillis,
        phaseObserver = phaseObserver,
    )

    suspend fun start(
        scope: CoroutineScope,
        ownerBareJid: String,
    ): SessionLifecycleRef = lifecycle.start(scope, ownerBareJid)

    suspend fun activateAttempt(
        sessionLifecycle: SessionLifecycleRef,
    ): AttemptActivation = lifecycle.activate(sessionLifecycle)

    suspend fun attachTransport(
        handle: ConnectionAttemptHandle,
        client: WaddleClientInterface,
    ): Boolean = lifecycle.attachTransport(handle, client)

    suspend fun disconnectTransport(
        handle: ConnectionAttemptHandle,
    ): Boolean = lifecycle.disconnectTransport(handle)

    suspend fun markReady(
        handle: ConnectionAttemptHandle,
        client: WaddleClientInterface,
        attempt: DeliveryAttemptRef,
    ): Boolean = lifecycle.markReady(handle, client, attempt)

    fun matches(
        handle: ConnectionAttemptHandle,
        attempt: DeliveryAttemptRef,
    ): Boolean = lifecycle.matches(handle, attempt)

    suspend fun beginShutdown(
        sessionLifecycle: SessionLifecycleRef,
    ): Boolean = lifecycle.beginShutdown(sessionLifecycle)

    suspend fun closeAttempt(
        handle: ConnectionAttemptHandle,
        producerQuiesced: Boolean,
    ): AttemptCloseOutcome = lifecycle.closeAttempt(handle, producerQuiesced)

    suspend fun shutdown(
        target: LifecycleShutdownTarget,
    ): LifecycleShutdownOutcome = lifecycle.shutdown(target)

    suspend fun recoverFencedTerminal(
        sessionLifecycle: SessionLifecycleRef,
    ): Boolean = lifecycle.recoverFencedTerminal(sessionLifecycle)

    suspend fun awaitStartupTerminalDrain(ownerBareJid: String) {
        lifecycle.awaitStartupTerminalDrain(ownerBareJid)
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
        val lease = when (
            val admission = lifecycle.acquireOutbound(
                source = source,
                expectedOwnerBareJid = expectedOwnerBareJid,
            )
        ) {
            is OutboundAdmissionResult.Granted -> admission.lease
            OutboundAdmissionResult.OwnerMismatch ->
                return SendResult(WaddleSendMessageOutcome.Error)
            OutboundAdmissionResult.LifecycleUnavailable ->
                return SendResult(WaddleSendMessageOutcome.NotConnected)
        }
        try {
            val draft = queuedMessage(
                owner = lease.lifecycle.ownerBareJid,
                source = source,
                payload = queuedPayload(conversationJid, isGroupchat, body, extras),
            )
            return when (lease) {
                is OutboundAdmissionLease.OfflineOutbound ->
                    enqueueOffline(draft)
                is OutboundAdmissionLease.LiveOutbound ->
                    sendLive(draft, lease)
                is OutboundAdmissionLease.Terminal ->
                    error("terminal lease cannot admit outbound messages")
            }
        } finally {
            lifecycle.releaseAdmission(lease)
        }
    }

    private suspend fun enqueueOffline(
        draft: QueuedOutboundDraft,
    ): SendResult {
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

    private suspend fun sendLive(
        draft: QueuedOutboundDraft,
        lease: OutboundAdmissionLease.LiveOutbound,
    ): SendResult {
        val admission = persistQueueMutation {
            journal.enqueueAndClaimAbsoluteHead(draft, lease.attempt)
        } ?: return SendResult(WaddleSendMessageOutcome.Error)
        return when (admission) {
            is LiveAdmissionResult.Claimed -> {
                val stored = admission.row
                val ownership = stored.ownership as? OutboundOwnership.NativeOwned
                    ?: return SendResult(WaddleSendMessageOutcome.Error)
                val outcome = sendMessage(stored, lease.client)
                reconcileInitialOutcome(stored, ownership, outcome)
            }
            is LiveAdmissionResult.Queued -> {
                lifecycle.signalDrain(lease.attempt)
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
        target = QueuedOutboundTarget.from(conversationJid, isGroupchat),
        content = QueuedOutboundContent(
            body = body,
            reply = QueuedOutboundReply(
                id = extras?.replyToId,
                authorJid = extras?.replyToAuthorJid,
                parentBody = extras?.replyParentBody,
            ),
            thread = QueuedOutboundThread(
                id = extras?.threadId,
                parent = extras?.threadParent,
            ),
            sharedFiles = extras?.sharedFiles.orEmpty(),
            mentions = extras?.mentions.orEmpty(),
        ),
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

        lifecycle.submitTerminal(
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
        val active = lifecycle.active() ?: return
        drainOutboundQueue(active.lifecycle, active.handle, active.attempt)
    }

    private suspend fun drainOutboundQueue(
        sessionLifecycle: SessionLifecycleRef,
        handle: ConnectionAttemptHandle,
        expectedAttempt: DeliveryAttemptRef,
    ) {
        val lease =
            lifecycle.acquireDrain(sessionLifecycle, handle, expectedAttempt)
                ?: return
        try {
            drainMutex.withLock {
                if (!lifecycle.matches(handle, expectedAttempt)) return@withLock
                awaitStartupTerminalDrain(sessionLifecycle.ownerBareJid)
                while (lifecycle.matches(handle, expectedAttempt)) {
                    val claimed =
                        journal.claimAbsoluteReadyHead(
                            sessionLifecycle.ownerBareJid,
                            expectedAttempt,
                        ) ?: return@withLock
                    val ownership = claimed.ownership as OutboundOwnership.NativeOwned
                    when (val outcome = sendMessage(claimed, lease.client)) {
                        is WaddleSendMessageOutcome.Sent -> {
                            if (outcome.stanzaId != claimed.clientStanzaId) {
                                lifecycle.submitTerminal(
                                    sessionLifecycle.ownerBareJid,
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
                            lifecycle.submitTerminal(
                                sessionLifecycle.ownerBareJid,
                                claimed.clientStanzaId,
                                ownership.attempt,
                                DeliveryTerminalKind.NONRETRYABLE_DELETE,
                            )
                        }
                    }
                }
            }
        } finally {
            lifecycle.releaseAdmission(lease)
        }
    }

    /**
     * Commit the exact Rust-minted RESUME -> FRESH_FALLBACK transition
     * before the connection loop polls another native event.
     */
    suspend fun rotateAndAwait(
        handle: ConnectionAttemptHandle,
        transition: DeliveryAttemptTransition,
        affectedStanzaIds: Set<String>,
    ): ResumeHandoffOutcome =
        lifecycle.rotate(handle, transition, affectedStanzaIds)

    /** Apply native terminal signals before asking Rust for another event. */
    suspend fun reconcileDeliveryEvent(event: XmppEvent): Boolean =
        when (event) {
            is XmppEvent.NativeDeliveryAcked -> {
                if (!reconcileTerminalEvent(event.attempt, event.clientStanzaId, DeliveryTerminalKind.ACK)) {
                    return false
                }
                drainOutboundQueue()
                false
            }
            is XmppEvent.NativeDeliveryFailed -> {
                if (
                    !reconcileTerminalEvent(
                        event.attempt,
                        event.clientStanzaId,
                        DeliveryTerminalKind.NATIVE_FAILURE,
                    )
                ) {
                    return false
                }
                drainOutboundQueue()
                false
            }
            else -> true
        }

    private suspend fun reconcileTerminalEvent(
        attempt: DeliveryAttemptRef,
        clientStanzaId: String,
        kind: DeliveryTerminalKind,
    ): Boolean {
        val lease = lifecycle.acquireTerminal(attempt) ?: return false
        return try {
            lifecycle.submitTerminal(
                attempt.ownerBareJid,
                clientStanzaId,
                attempt,
                kind,
            )
            true
        } finally {
            lifecycle.releaseAdmission(lease)
        }
    }

    private suspend fun sendMessage(
        queued: QueuedOutboundMessage,
        client: WaddleClientInterface,
    ): WaddleSendMessageOutcome {
        val (finalBody, options) = preparedSend(
            queued.clientStanzaId,
            queued.body,
            queued.sendExtras(),
        )
        val outcome = try {
            if (queued.isGroupchat) {
                client.sendGroupchatMessage(queued.conversationJid, finalBody, options)
            } else {
                client.sendChatMessage(queued.conversationJid, finalBody, options)
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            WaddleSendMessageOutcome.TransportError
        }
        if (!queued.isGroupchat && outcome is WaddleSendMessageOutcome.Sent) {
            stores.timelineStore.onLiveMessage(
                ownDmEcho(
                    ownJid = queued.ownerBareJid,
                    peerJid = queued.conversationJid,
                    stanzaId = queued.clientStanzaId,
                    body = finalBody,
                    options = options,
                ),
            )
        }
        return outcome
    }

    private fun isQueueableFailure(outcome: WaddleSendMessageOutcome): Boolean =
        outcome == WaddleSendMessageOutcome.NotConnected ||
            outcome == WaddleSendMessageOutcome.TransportError
}
