package social.waddle.android.client

import kotlinx.coroutines.CoroutineScope
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryAttemptTransition
import social.waddle.android.client.prefs.DeliveryOwnerBareJid
import social.waddle.android.client.prefs.DeliverySource
import social.waddle.android.client.prefs.DeliveryTerminalKind
import social.waddle.android.client.prefs.QueuedOutboundContent
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
    private val journal: DeliveryJournalStore,
    private val resume: ResumePersistence,
    private val dispatchEvent: (XmppEvent) -> Unit,
    transitionTimeoutMillis: Long = 5_000L,
    phaseObserver: OutboundLifecyclePhaseObserver =
        OutboundLifecyclePhaseObserver.NONE,
    ownerFinalizer: (suspend (OwnerWorkers, SessionLifecycleRef, AttemptRecord?) -> OwnerFinalizationResult)? = null,
    workerStartHooks: WorkerStartHooks = WorkerStartHooks.None,
    private val workerExitEvidence: WorkerExitEvidence,
    outboundDrain: (suspend (SessionLifecycleRef, ConnectionAttemptHandle, DeliveryAttemptRef) -> Unit)? = null,
    private val admissionReleaseOperations: OutboundAdmissionReleaseOperations =
        OutboundAdmissionReleaseOperations.COORDINATOR,
) {
    private val sendService = OutboundSendService(journal, stores.timelineStore)
    private val drainService = OutboundDrainService(journal, sendService)
    private val lifecycle = OutboundLifecycleStateStore(
        activeSession = activeSession,
        journal = journal,
        resume = resume,
        dispatchEvent = dispatchEvent,
        drain = outboundDrain ?: ::drainDeliveryJournal,
        transitionTimeoutMillis = transitionTimeoutMillis,
        phaseObserver = phaseObserver,
        ownerFinalizer = ownerFinalizer,
        workerStartHooks = workerStartHooks,
        workerExitEvidence = workerExitEvidence,
    )

    internal fun workerRecoveryException(outcome: WorkerRecoveryOutcome): WorkerRecoveryException =
        WorkerRecoveryException(outcome, workerExitEvidence.lookup(outcome))

    suspend fun start(
        scope: CoroutineScope,
        ownerBareJid: String,
    ): LifecycleStartResult = lifecycle.start(scope, ownerBareJid)

    suspend fun activateAttempt(
        sessionLifecycle: SessionLifecycleRef,
    ): AttemptActivation = lifecycle.activate(sessionLifecycle)

    suspend fun attachTransport(
        handle: ConnectionAttemptHandle,
        client: WaddleClientInterface,
    ): Boolean = lifecycle.attachTransport(handle, client)

    suspend fun beginTransportConstruction(
        handle: ConnectionAttemptHandle,
    ): TransportConstructionClaim? = lifecycle.beginTransportConstruction(handle)

    suspend fun attachConstructedTransport(
        claim: TransportConstructionClaim,
        client: WaddleClientInterface,
    ): TransportAttachOutcome = lifecycle.attachConstructedTransport(claim, client)

    suspend fun finishSupersededConstruction(claim: TransportConstructionClaim) {
        lifecycle.finishSupersededConstruction(claim)
    }

    suspend fun markTransportClosed(
        handle: ConnectionAttemptHandle,
        closed: Boolean,
    ) {
        lifecycle.markTransportClosed(handle, closed)
    }

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
    ): BeginShutdownDecision = lifecycle.beginShutdown(sessionLifecycle)

    suspend fun closeAttempt(
        handle: ConnectionAttemptHandle,
        producerQuiesced: Boolean,
    ): AttemptCloseOutcome = lifecycle.closeAttempt(handle, producerQuiesced)

    suspend fun shutdown(
        target: LifecycleShutdownTarget,
    ): LifecycleShutdownOutcome = lifecycle.shutdown(target)

    suspend fun recoverFencedWorkers(
        sessionLifecycle: SessionLifecycleRef,
    ): WorkerRecoveryOutcome = lifecycle.recoverFencedWorkers(sessionLifecycle)

    suspend fun awaitStartupTerminalDrain(ownerBareJid: String) {
        lifecycle.awaitStartupTerminalDrain(ownerBareJid)
    }

    internal fun signalDrain(attempt: DeliveryAttemptRef): DrainSignalOutcome =
        lifecycle.signalDrain(attempt)

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
        var primary: Throwable? = null
        try {
            val request = OutboundSendRequest(
                target = QueuedOutboundTarget.from(conversationJid, isGroupchat),
                content = queuedContent(body, extras),
                source = source,
            )
            return when (lease) {
                is OutboundAdmissionLease.OfflineOutbound -> when (
                    val disposition = sendService.send(
                        request,
                        OutboundSendAdmission.Offline(DeliveryOwnerBareJid(lease.lifecycle.ownerBareJid)),
                    )
                ) {
                    is OutboundSendDisposition.Completed -> disposition.result
                    is OutboundSendDisposition.Queued,
                    is OutboundSendDisposition.TerminalRequired,
                    -> error("offline admission cannot produce $disposition")
                }
                is OutboundAdmissionLease.LiveOutbound -> when (
                    val disposition = sendService.send(
                        request,
                        OutboundSendAdmission.Live(
                            owner = DeliveryOwnerBareJid(lease.lifecycle.ownerBareJid),
                            attempt = lease.attempt,
                            client = lease.client,
                        ),
                    )
                ) {
                    is OutboundSendDisposition.Completed -> disposition.result
                    is OutboundSendDisposition.Queued -> {
                        lifecycle.signalDrain(lease.attempt)
                        disposition.result
                    }
                    is OutboundSendDisposition.TerminalRequired -> {
                        requireTerminalCommitted(
                            lifecycle.submitTerminal(
                                disposition.row.ownerBareJid,
                                disposition.row.clientStanzaId,
                                disposition.ownership.attempt,
                                DeliveryTerminalKind.NONRETRYABLE_DELETE,
                            ),
                        )
                        drainDeliveryJournal()
                        SendResult(
                            if (disposition.wireOutcome is WaddleSendMessageOutcome.Sent) {
                                WaddleSendMessageOutcome.Error
                            } else {
                                disposition.wireOutcome
                            },
                        )
                    }
                }
                is OutboundAdmissionLease.Terminal ->
                    error("terminal lease cannot admit outbound messages")
            }
        } catch (failure: Throwable) {
            primary = failure
            throw failure
        } finally {
            requireLifecycleRelease(
                admissionReleaseOperations.release(lifecycle, lease),
                lease.capability,
                when (lease) {
                    is OutboundAdmissionLease.OfflineOutbound -> LifecycleReleaseSite.OFFLINE_OUTBOUND
                    is OutboundAdmissionLease.LiveOutbound -> LifecycleReleaseSite.LIVE_OUTBOUND
                    is OutboundAdmissionLease.Terminal -> LifecycleReleaseSite.TERMINAL_COMMAND
                },
                primary = primary,
            )
        }
    }

    private fun queuedContent(
        body: String,
        extras: MessageSendExtras?,
    ): QueuedOutboundContent = QueuedOutboundContent(
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
        )

    /**
     * Replay owner Ready rows after the startup terminal barrier. Rows already
     * native-owned or terminal are never selected.
     */
    suspend fun drainDeliveryJournal() {
        val active = lifecycle.active() ?: return
        drainDeliveryJournal(active.lifecycle, active.handle, active.attempt)
    }

    private suspend fun drainDeliveryJournal(
        sessionLifecycle: SessionLifecycleRef,
        handle: ConnectionAttemptHandle,
        expectedAttempt: DeliveryAttemptRef,
    ) {
        val lease =
            lifecycle.acquireDrain(sessionLifecycle, handle, expectedAttempt)
                ?: return
        var primary: Throwable? = null
        try {
            val critical = lifecycle.acquireDrainCriticalSection()
            try {
                if (!lifecycle.matches(handle, expectedAttempt)) return
                awaitStartupTerminalDrain(sessionLifecycle.ownerBareJid)
                while (lifecycle.matches(handle, expectedAttempt)) {
                    when (
                        val disposition = drainService.drainOne(
                            OutboundDrainOperation(
                                owner = DeliveryOwnerBareJid(sessionLifecycle.ownerBareJid),
                                attempt = expectedAttempt,
                                client = lease.client,
                            ),
                        )
                    ) {
                        OutboundDrainDisposition.NoReady,
                        OutboundDrainDisposition.AwaitingNativeAck,
                        OutboundDrainDisposition.RetryableReleased,
                        -> return
                        is OutboundDrainDisposition.TerminalRequired -> {
                            requireTerminalCommitted(
                                lifecycle.submitTerminal(
                                    disposition.row.ownerBareJid,
                                    disposition.row.clientStanzaId,
                                    disposition.ownership.attempt,
                                    DeliveryTerminalKind.NONRETRYABLE_DELETE,
                                ),
                            )
                        }
                    }
                }
            } finally {
                lifecycle.releaseDrainCriticalSection(critical)
            }
        } catch (failure: Throwable) {
            primary = failure
            throw failure
        } finally {
            requireLifecycleRelease(
                admissionReleaseOperations.release(lifecycle, lease),
                lease.capability,
                LifecycleReleaseSite.OUTBOUND_DRAIN,
                primary = primary,
            )
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
                drainDeliveryJournal()
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
                drainDeliveryJournal()
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
        var primary: Throwable? = null
        return try {
            requireTerminalCommitted(
                lifecycle.submitTerminal(
                    attempt.ownerBareJid,
                    clientStanzaId,
                    attempt,
                    kind,
                ),
            )
            true
        } catch (failure: Throwable) {
            primary = failure
            throw failure
        } finally {
            requireLifecycleRelease(
                admissionReleaseOperations.release(lifecycle, lease),
                lease.capability,
                LifecycleReleaseSite.TERMINAL_COMMAND,
                primary = primary,
            )
        }
    }
}
