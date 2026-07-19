package social.waddle.android.client

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryAttemptTransition
import social.waddle.android.client.prefs.DeliverySource
import social.waddle.android.client.prefs.DeliveryTerminalKind
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.session.ResumePersistence
import social.waddle.client.ffi.WaddleClientInterface
import java.util.UUID

/**
 * The sole authority for one authenticated owner's outbound lifecycle.
 *
 * The mutex protects only immutable state transitions and admission records.
 * Durable storage, worker joins, and native FFI always run after it is
 * released. Handoff and Closing deny new leases while existing operations
 * quiesce under a bounded wait.
 */
internal class OutboundLifecycleCoordinator(
    private val activeSession: ActiveSession,
    private val journal: OutboundQueue,
    private val resume: ResumePersistence,
    dispatchEvent: (XmppEvent) -> Unit,
    drain: suspend (
        SessionLifecycleRef,
        ConnectionAttemptHandle,
        DeliveryAttemptRef,
    ) -> Unit,
    private val transitionTimeoutMillis: Long = TRANSITION_TIMEOUT_MILLIS,
    private val phaseObserver: OutboundLifecyclePhaseObserver =
        OutboundLifecyclePhaseObserver.NONE,
) {
    private val gate = Mutex()
    private val drainWorker = OutboundDrainWorker(drain)
    private val terminalWorker = DeliveryTerminalWorker(journal, dispatchEvent)
    private val phaseOperations = OutboundLifecyclePhaseOperations(
        activeSession,
        journal,
        phaseObserver,
        resume,
    )
    private val finalizationOperations = OutboundLifecycleFinalizationOperations(
        activeSession,
        journal,
        resume,
        drainWorker,
        terminalWorker,
        transitionTimeoutMillis,
    )
    /** Replaced on every start; old completions cannot affect a new owner. */
    private var retainedOperations: LifecycleOperationRegistry? = null
    private var ownerWorkers: OwnerWorkers? = null
    private var rotationMutationLease: RotationMutationLease? = null
    private var currentAttempt: AttemptRecord? = null
    private var lastClosedAttempt: AttemptRecord? = null
    private var pendingShutdown: LifecycleShutdownOutcome.FencedWithPending? = null
    private var recoveryClaim: WorkerRecoveryClaim? = null

    @Volatile
    private var state: OutboundLifecycleState = OutboundLifecycleState.Stopped

    suspend fun start(
        scope: CoroutineScope,
        ownerBareJid: String,
    ): SessionLifecycleRef {
        // Mint identity before the first suspension so replacement can never
        // inherit a partially-created generation.
        val lifecycle = SessionLifecycleRef.create(ownerBareJid)
        val workers = gate.withLock {
            check(state == OutboundLifecycleState.Stopped) {
                "outbound lifecycle is not restartable from $state"
            }
            val createdWorkers = OwnerWorkers(lifecycle)
            ownerWorkers = createdWorkers
            retainedOperations = LifecycleOperationRegistry(lifecycle)
            rotationMutationLease = null
            currentAttempt = null
            lastClosedAttempt = null
            pendingShutdown = null
            recoveryClaim = null
            state = OutboundLifecycleState.Bootstrapping(lifecycle)
            createdWorkers
        }
        finalizationOperations.startWorkers(
            scope = scope,
            workers = workers,
            onReady = ::onWorkerReady,
            onExit = ::onWorkerExit,
        )
        workers.terminal.awaitReady()
        workers.drain.awaitReady()
        gate.withLock {
            check(state == OutboundLifecycleState.Bootstrapping(lifecycle) && workers.bothReady()) {
                "worker startup lost lifecycle authority"
            }
            state = OutboundLifecycleState.Open(lifecycle)
        }
        return lifecycle
    }

    private suspend fun onWorkerReady(ownership: WorkerOwnership) {
        gate.withLock {
            val workers = ownerWorkers ?: return@withLock
            if (workers.lifecycle != ownership.lifecycle || !workers.markReady(ownership)) return@withLock
        }
    }

    /** Callback completes only after this exact exit has closed future admission. */
    private suspend fun onWorkerExit(exit: WorkerExit) {
        gate.withLock {
            val workers = ownerWorkers ?: return@withLock
            if (workers.lifecycle != exit.lifecycle || !workers.recordExactExit(exit)) return@withLock
            val ordinaryStop = state is OutboundLifecycleState.Closing &&
                exit.reason is WorkerExitReason.RequestedStop
            if (!ordinaryStop && exit.reason !is WorkerExitReason.RequestedStop) {
                retainedOperations?.closeAdmissions()
                state = OutboundLifecycleState.Fenced(
                    exit.lifecycle,
                    LifecycleFenceCause.WorkerExited(WorkerFence(exit)),
                )
            } else if ((state as? OutboundLifecycleState.Fenced)?.cause
                    .let { it as? LifecycleFenceCause.AwaitingRequestedWorkerExit }
                    ?.ownership == exit.ownership()
            ) {
                state = OutboundLifecycleState.Fenced(
                    exit.lifecycle,
                    LifecycleFenceCause.WorkerExited(WorkerFence(exit)),
                )
            }
        }
    }

    suspend fun activate(lifecycle: SessionLifecycleRef): AttemptActivation {
        val handle = ConnectionAttemptHandle.random()
        val claimed = claimActivation(lifecycle, handle)
        check(claimed) { "outbound lifecycle is not open for activation" }

        var attempt: DeliveryAttemptRef? = null
        try {
            val bootstrap = phaseOperations.journalActivation(lifecycle)
            attempt = bootstrap.attempt
            recordActivation(lifecycle, handle, bootstrap.attempt)
            val active = phaseOperations.publishActivation(requireNotNull(ownerWorkers), lifecycle, handle, bootstrap)
            publishActivation(lifecycle, handle, bootstrap.attempt)
            phaseOperations.attemptPublished()
            return AttemptActivation(lifecycle, handle, bootstrap, active.bridge)
        } catch (failure: Throwable) {
            val compensated = compensateActivation(lifecycle, handle, attempt)
            if (!compensated) {
                throw LifecycleTransitionException(
                    lifecycle = lifecycle,
                    component = LifecyclePendingComponent.ACTIVATION_COMPENSATION,
                    pending = 1,
                    cause = failure,
                )
            }
            throw failure
        }
    }

    private suspend fun claimActivation(
        lifecycle: SessionLifecycleRef,
        handle: ConnectionAttemptHandle,
    ): Boolean = gate.withLock {
        if (state != OutboundLifecycleState.Open(lifecycle)) return@withLock false
        state = OutboundLifecycleState.Handoff(
            lifecycle = lifecycle,
            handle = handle,
            previousAttempt = null,
            nextAttempt = null,
        )
        true
    }

    private suspend fun recordActivation(
        lifecycle: SessionLifecycleRef,
        handle: ConnectionAttemptHandle,
        attempt: DeliveryAttemptRef,
    ) = gate.withLock {
        val handoff = state as? OutboundLifecycleState.Handoff
        check(handoff?.lifecycle == lifecycle && handoff.handle == handle) {
            "activation lost lifecycle authority"
        }
        currentAttempt = AttemptRecord(lifecycle, handle, attempt)
        state = handoff.copy(nextAttempt = attempt)
    }

    private suspend fun publishActivation(
        lifecycle: SessionLifecycleRef,
        handle: ConnectionAttemptHandle,
        attempt: DeliveryAttemptRef,
    ) = gate.withLock {
        val handoff = state as? OutboundLifecycleState.Handoff
        check(
            handoff?.lifecycle == lifecycle &&
                handoff.handle == handle &&
                handoff.nextAttempt == attempt,
        ) {
            "activation was fenced before publication"
        }
        state = OutboundLifecycleState.Active(lifecycle, handle, attempt)
    }

    suspend fun attachTransport(
        handle: ConnectionAttemptHandle,
        client: WaddleClientInterface,
    ): Boolean = gate.withLock {
        val record = currentAttempt
        if (
            record?.handle != handle ||
            state.lifecycleOrNull() != record.lifecycle ||
            state !is OutboundLifecycleState.Active
        ) {
            return@withLock false
        }
        record.client = client
        true
    }

    /**
     * Retain the exact active attempt before invoking ClientFactory.create.
     * Shutdown therefore fences and reports the in-flight construction rather
     * than claiming the owner is stopped while a native client is materialized.
     */
    suspend fun beginTransportConstruction(
        handle: ConnectionAttemptHandle,
    ): TransportConstructionClaim? = gate.withLock {
        val active = state as? OutboundLifecycleState.Active ?: return@withLock null
        val record = currentAttempt
        if (record?.handle != handle || record.attempt != active.attempt) {
            return@withLock null
        }
        val capability = OperationCapability(
            lifecycle = active.lifecycle,
            attempt = active.attempt,
            operationId = UUID.randomUUID(),
        )
        if (retainedOperations?.retain(capability) != true) return@withLock null
        TransportConstructionClaim(active.lifecycle, handle, active.attempt, capability)
    }

    /**
     * Attach transfers ownership to the exact attempt. A fenced construction
     * remains retained until the caller has closed its unowned client.
     */
    suspend fun attachConstructedTransport(
        claim: TransportConstructionClaim,
        client: WaddleClientInterface,
    ): TransportAttachOutcome = gate.withLock {
        val active = state as? OutboundLifecycleState.Active
        val record = currentAttempt
        val attached =
            active?.lifecycle == claim.lifecycle &&
                active.handle == claim.handle &&
                active.attempt == claim.attempt &&
                record?.handle == claim.handle &&
                retainedOperations?.owns(claim.capability) == true
        if (!attached) return@withLock TransportAttachOutcome.SupersededAndClose
        record.client = client
        record.requiresClientCloseProof = true
        retainedOperations?.release(claim.capability)
        TransportAttachOutcome.Attached
    }

    /** Complete the retained construction only after its unowned client closes. */
    suspend fun finishSupersededConstruction(claim: TransportConstructionClaim) {
        gate.withLock { retainedOperations?.release(claim.capability) }
    }

    /** Production close proof; completion is idempotent for one exact handle. */
    suspend fun markTransportClosed(
        handle: ConnectionAttemptHandle,
        closed: Boolean,
    ) {
        gate.withLock {
            currentAttempt
                ?.takeIf { it.handle == handle }
                ?.clientClosed
                ?.complete(closed)
        }
    }

    suspend fun disconnectTransport(
        handle: ConnectionAttemptHandle,
    ): Boolean =
        claimDisconnect(handle)?.let {
            finalizationOperations.disconnect(it)
        } ?: false

    private suspend fun claimDisconnect(
        handle: ConnectionAttemptHandle,
    ): DisconnectClaim? = gate.withLock {
        val record = currentAttempt
        if (record?.handle != handle) return@withLock null
        if (record.disconnectStarted) {
            DisconnectClaim.Wait(record.disconnectResult)
        } else {
            record.disconnectStarted = true
            DisconnectClaim.Execute(record, record.disconnectResult)
        }
    }

    suspend fun markReady(
        handle: ConnectionAttemptHandle,
        readyClient: WaddleClientInterface,
        expectedAttempt: DeliveryAttemptRef,
    ): Boolean {
        val allowed = gate.withLock {
            val active = state as? OutboundLifecycleState.Active
            active?.handle == handle && active.attempt == expectedAttempt
        }
        if (!allowed) return false
        activeSession.onReady(readyClient, expectedAttempt)
        val stillAllowed = matches(handle, expectedAttempt)
        if (!stillAllowed) activeSession.endAttempt(expectedAttempt)
        return stillAllowed
    }

    fun matches(
        handle: ConnectionAttemptHandle,
        attempt: DeliveryAttemptRef,
    ): Boolean =
        (state as? OutboundLifecycleState.Active)
            ?.let { it.handle == handle && it.attempt == attempt } == true

    fun active(): OutboundLifecycleState.Active? =
        state as? OutboundLifecycleState.Active

    suspend fun beginShutdown(lifecycle: SessionLifecycleRef): Boolean =
        gate.withLock {
            if (state.lifecycleOrNull() != lifecycle) return@withLock false
            retainedOperations?.closeAdmissions()
            val record = currentAttempt
            state = OutboundLifecycleState.Closing(
                lifecycle = lifecycle,
                handle = record?.handle,
                attempt = record?.attempt ?: state.attemptOrNull(),
            )
            true
        }

    suspend fun acquireOutbound(
        source: DeliverySource,
        expectedOwnerBareJid: String? = null,
    ): OutboundAdmissionResult {
        val claim = claimOutboundReservation(expectedOwnerBareJid)
        val reservation = when (claim) {
            is OutboundReservationClaim.Granted -> claim.reservation
            OutboundReservationClaim.OwnerMismatch ->
                return OutboundAdmissionResult.OwnerMismatch
            OutboundReservationClaim.LifecycleUnavailable ->
                return OutboundAdmissionResult.LifecycleUnavailable
        }
        return materializeOutboundAdmission(activeSession, reservation, source)
    }

    private suspend fun claimOutboundReservation(
        expectedOwnerBareJid: String?,
    ): OutboundReservationClaim = gate.withLock {
        val claim = classifyOutboundReservation(state, expectedOwnerBareJid)
        if (claim is OutboundReservationClaim.Granted) {
            val reservation = claim.reservation
            retainReservation(reservation)
        }
        claim
    }

    suspend fun acquireDrain(
        lifecycle: SessionLifecycleRef,
        handle: ConnectionAttemptHandle,
        expectedAttempt: DeliveryAttemptRef,
    ): OutboundAdmissionLease.LiveOutbound? {
        if (!matches(handle, expectedAttempt)) return null
        val reservation = reserveAdmission(
            expectedOwnerBareJid = lifecycle.ownerBareJid,
            expectedAttempt = expectedAttempt,
            requireActive = true,
        ) ?: return null
        val client = activeSession.clientAtAttempt(expectedAttempt)
        if (client == null) {
            releaseReservation(
                reservation.token,
                reservation.lifecycle,
                reservation.attempt,
            )
            return null
        }
        return OutboundAdmissionLease.LiveOutbound(
            lifecycle = reservation.lifecycle,
            attempt = expectedAttempt,
            client = client,
            purpose = LiveOutboundPurpose.Drain,
            token = reservation.token,
        )
    }

    suspend fun acquireTerminal(
        expectedAttempt: DeliveryAttemptRef,
    ): OutboundAdmissionLease.Terminal? {
        val reservation = reserveAdmission(
            expectedOwnerBareJid = expectedAttempt.ownerBareJid,
            expectedAttempt = expectedAttempt,
            requireActive = true,
        ) ?: return null
        return OutboundAdmissionLease.Terminal(
            lifecycle = reservation.lifecycle,
            attempt = expectedAttempt,
            token = reservation.token,
        )
    }

    private suspend fun reserveAdmission(
        expectedOwnerBareJid: String?,
        expectedAttempt: DeliveryAttemptRef?,
        requireActive: Boolean,
    ): AdmissionReservation? = gate.withLock {
        val reservation = createAdmissionReservation(
            state,
            expectedOwnerBareJid,
            expectedAttempt,
            requireActive,
        ) ?: return@withLock null
        retainReservation(reservation)
        reservation
    }

    suspend fun releaseAdmission(lease: OutboundAdmissionLease): LifecycleReleaseOutcome =
        releaseReservation(
            token = lease.token,
            lifecycle = lease.lifecycle,
            attempt = when (lease) {
                is OutboundAdmissionLease.OfflineOutbound -> lease.attempt
                is OutboundAdmissionLease.LiveOutbound -> lease.attempt
                is OutboundAdmissionLease.Terminal -> lease.attempt
            },
        )

    private suspend fun releaseReservation(
        token: UUID,
        lifecycle: SessionLifecycleRef,
        attempt: DeliveryAttemptRef?,
    ): LifecycleReleaseOutcome =
        gate.withLock {
            retainedOperations?.release(
                OperationCapability(lifecycle, attempt, token),
            ) ?: LifecycleReleaseOutcome.NotOwned
        }

    suspend fun rotate(
        handle: ConnectionAttemptHandle,
        transition: DeliveryAttemptTransition,
        affectedStanzaIds: Set<String>,
    ): ResumeHandoffOutcome {
        val lease = gate.withLock {
            val active = state as? OutboundLifecycleState.Active
            if (
                active?.handle != handle ||
                active.attempt != transition.old ||
                active.lifecycle.ownerBareJid != transition.old.ownerBareJid
            ) {
                return ResumeHandoffOutcome.Rejected
            }
            val capability = OperationCapability(
                lifecycle = active.lifecycle,
                attempt = transition.old,
                operationId = UUID.randomUUID(),
            )
            check(retainedOperations?.retain(capability) == true) {
                "outbound lifecycle lost operation registry"
            }
            val acquired = RotationMutationLease(
                lifecycle = active.lifecycle,
                handle = handle,
                old = transition.old,
                fresh = transition.fresh,
                capability = capability,
            )
            rotationMutationLease = acquired
            state = OutboundLifecycleState.Handoff(
                lifecycle = active.lifecycle,
                handle = handle,
                previousAttempt = transition.old,
                nextAttempt = transition.fresh,
            )
            acquired
        }
        return try {
            performRotation(
                lease,
                affectedStanzaIds,
            )
        } catch (failure: Throwable) {
            val stillHandoff = gate.withLock {
                val handoff = state as? OutboundLifecycleState.Handoff
                handoff?.lifecycle == lease.lifecycle && handoff.handle == lease.handle
            }
            if (stillHandoff) {
                compensateHandoff(lease.lifecycle, lease.handle, transition)
            }
            releaseRotationMutation(lease)
            throw failure
        }
    }

    private suspend fun performRotation(
        lease: RotationMutationLease,
        affectedStanzaIds: Set<String>,
    ): ResumeHandoffOutcome {
        if (!awaitOtherOperations(lease.capability)) {
            transitionToClosing(lease.lifecycle, lease.handle, lease.old)
            throw LifecycleTransitionException(
                lease.lifecycle,
                LifecyclePendingComponent.ATTEMPT_LEASES,
                pendingLeaseCount(),
            )
        }

        val journalOutcome =
            phaseOperations.journalRotation(
                DeliveryAttemptTransition(lease.old, lease.fresh),
                affectedStanzaIds,
            )
        val smVersion = when (journalOutcome) {
            is RotationJournalOutcome.Accepted -> journalOutcome.smVersion
            RotationJournalOutcome.Rejected -> {
                gate.withLock {
                    val handoff = state as? OutboundLifecycleState.Handoff
                    if (
                        handoff?.lifecycle == lease.lifecycle &&
                            handoff.handle == lease.handle &&
                            rotationMutationLease == lease
                    ) {
                        state = OutboundLifecycleState.Active(
                            lease.lifecycle,
                            lease.handle,
                            lease.old,
                        )
                    }
                }
                releaseRotationMutation(lease)
                return ResumeHandoffOutcome.Rejected
            }
        }

        if (
            !phaseOperations.publishRotation(
                requireNotNull(ownerWorkers),
                lease.lifecycle,
                lease.handle,
                DeliveryAttemptTransition(lease.old, lease.fresh),
                smVersion,
            )
        ) {
            try {
                compensateHandoff(
                    lease.lifecycle,
                    lease.handle,
                    DeliveryAttemptTransition(lease.old, lease.fresh),
                )
            } finally {
                releaseRotationMutation(lease)
            }
            return ResumeHandoffOutcome.Rejected
        }
        gate.withLock {
            val handoff = state as? OutboundLifecycleState.Handoff
            check(
                handoff?.lifecycle == lease.lifecycle &&
                    handoff.handle == lease.handle &&
                    rotationMutationLease == lease,
            ) {
                "resume handoff lost lifecycle authority"
            }
            currentAttempt?.attempt = lease.fresh
            state = OutboundLifecycleState.Active(lease.lifecycle, lease.handle, lease.fresh)
        }
        phaseOperations.rotationPublished()
        releaseRotationMutation(lease)
        return ResumeHandoffOutcome.Committed
    }

    suspend fun closeAttempt(
        handle: ConnectionAttemptHandle,
        producerQuiesced: Boolean,
    ): AttemptCloseOutcome {
        val claimed = gate.withLock {
            val decision = decideAttemptClose(
                state,
                currentAttempt,
                lastClosedAttempt,
                handle,
                producerQuiesced,
            )
            if (decision.completeProducer) {
                currentAttempt?.producerStopped?.complete(Unit)
            }
            decision.nextState?.let { state = it }
            decision.claim
        }
        when (claimed) {
            CloseClaim.AlreadyClosed -> return AttemptCloseOutcome.AlreadyClosed
            CloseClaim.SessionShutdown -> return AttemptCloseOutcome.OwnedBySessionShutdown
            CloseClaim.Stale -> return AttemptCloseOutcome.Stale
            is CloseClaim.Owned -> Unit
        }
        val record = claimed.record
        val preparation =
            finalizationOperations.prepareAttemptClose(record, producerQuiesced)
        if (preparation != null) return preparation
        if (!finalizationOperations.transportClosed(record)) {
            return AttemptCloseOutcome.FencedWithPending(
                LifecyclePendingComponent.NATIVE_CLIENT_CLOSE,
                1,
            )
        }
        if (!awaitLeaseDrain()) {
            return AttemptCloseOutcome.FencedWithPending(
                LifecyclePendingComponent.ATTEMPT_LEASES,
                pendingLeaseCount(),
            )
        }
        val workers = gate.withLock { ownerWorkers }
            ?: return AttemptCloseOutcome.FencedWithPending(
                LifecyclePendingComponent.OUTBOUND_DRAIN,
                1,
            )
        val finalized = finalizationOperations.finalizeAttemptClose(workers, record)
        if (finalized != AttemptCloseOutcome.Closed) return finalized
        gate.withLock {
            if (currentAttempt === record && state is OutboundLifecycleState.Closing) {
                lastClosedAttempt = record
                currentAttempt = null
                state = OutboundLifecycleState.Open(record.lifecycle)
            }
        }
        return AttemptCloseOutcome.Closed
    }

    suspend fun shutdown(
        target: LifecycleShutdownTarget,
    ): LifecycleShutdownOutcome {
        if (target is LifecycleShutdownTarget.ExactAttempt) {
            val handle = gate.withLock {
                val active = state as? OutboundLifecycleState.Active
                if (
                    active?.lifecycle != target.lifecycle ||
                    active.attempt != target.attempt
                ) {
                    return LifecycleShutdownOutcome.Stale
                }
                active.handle
            }
            return closeAttempt(handle, producerQuiesced = true)
                .toShutdownOutcome(target.lifecycle)
        }

        val lifecycle = target.lifecycle
        val record = gate.withLock {
            if (state.lifecycleOrNull() != lifecycle) {
                return LifecycleShutdownOutcome.Stale
            }
            retainedOperations?.closeAdmissions()
            val activeRecord = currentAttempt
            state = OutboundLifecycleState.Closing(
                lifecycle = lifecycle,
                handle = activeRecord?.handle,
                attempt = activeRecord?.attempt ?: state.attemptOrNull(),
            )
            activeRecord
        }

        val shutdown = withContext(NonCancellable) {
            stopCurrentOwner(lifecycle, record)
        }
        if (shutdown != null) {
            gate.withLock {
                pendingShutdown = shutdown
                val workers = ownerWorkers
                val awaiting = when (shutdown.component) {
                    LifecyclePendingComponent.TERMINAL_DRAIN -> workers?.terminalOwnership
                    LifecyclePendingComponent.OUTBOUND_DRAIN -> workers?.drainOwnership
                    else -> null
                }
                if (awaiting != null) {
                    retainedOperations?.closeAdmissions()
                    state = OutboundLifecycleState.Fenced(
                        lifecycle,
                        LifecycleFenceCause.AwaitingRequestedWorkerExit(awaiting),
                    )
                }
            }
            return shutdown
        }

        gate.withLock {
            currentAttempt = null
            lastClosedAttempt = null
            pendingShutdown = null
            ownerWorkers = null
            state = OutboundLifecycleState.Stopped
        }
        return LifecycleShutdownOutcome.Stopped
    }

    suspend fun recoverFencedWorkers(lifecycle: SessionLifecycleRef): WorkerRecoveryOutcome {
        val awaitingExit = gate.withLock {
            (state as? OutboundLifecycleState.Fenced)
                ?.takeIf { it.lifecycle == lifecycle }
                ?.cause is LifecycleFenceCause.AwaitingRequestedWorkerExit
        }
        if (awaitingExit) return WorkerRecoveryOutcome.WorkerExitPending
        val claimed = gate.withLock {
            val fenced = state as? OutboundLifecycleState.Fenced
                ?: return@withLock null
            if (fenced.lifecycle != lifecycle) return@withLock null
            val cause = fenced.cause as? LifecycleFenceCause.WorkerExited
                ?: return@withLock null
            val workers = ownerWorkers ?: return@withLock null
            if (!workers.owns(cause.fence.exit.ownership())) return@withLock WorkerRecoveryClaim(
                lifecycle,
                cause.fence,
                WorkerRecoveryToken.random(),
            ) to null
            if (recoveryClaim != null) return@withLock WorkerRecoveryClaim(
                lifecycle,
                cause.fence,
                WorkerRecoveryToken.random(),
            ) to workers
            retainedOperations?.closeAdmissions()
            WorkerRecoveryClaim(lifecycle, cause.fence, WorkerRecoveryToken.random()).also {
                recoveryClaim = it
            } to workers
        }
        if (claimed == null) return WorkerRecoveryOutcome.NotFenced
        val (claim, workers) = claimed
        if (workers == null) return WorkerRecoveryOutcome.OwnershipMismatch
        if (gate.withLock { recoveryClaim != claim }) return WorkerRecoveryOutcome.RecoveryInProgress

        workers.siblingOf(claim.fence.exit.ownership())?.let { sibling ->
            if (workers.exitFor(sibling) == null) {
                when (sibling.kind) {
                    WorkerKind.DELIVERY_TERMINAL -> workers.terminal.requestStop()
                    WorkerKind.OUTBOUND_DRAIN -> workers.drain.requestStop()
                }
            }
        }
        val exits = listOf(
            workers.terminal.awaitExit(transitionTimeoutMillis),
            workers.drain.awaitExit(transitionTimeoutMillis),
        )
        if (exits.any { it is WorkerAwaitOutcome.TimedOut }) {
            clearRecoveryClaim(claim)
            return WorkerRecoveryOutcome.WorkerExitPending
        }
        if (!awaitLeaseDrain()) {
            clearRecoveryClaim(claim)
            return WorkerRecoveryOutcome.RetainedOperationsPending
        }
        if (!ownsRecoveryClaim(claim, workers)) {
            return WorkerRecoveryOutcome.OwnershipMismatch
        }
        val cleanup = finalizationOperations.recoverDurableState(workers, lifecycle)
        if (cleanup is OwnerFinalizationResult.Pending) {
            clearRecoveryClaim(claim)
            return WorkerRecoveryOutcome.DurableCleanupPending(cleanup.component, cleanup.count)
        }
        return gate.withLock {
            if (!ownsRecoveryClaimLocked(claim, workers)) return@withLock WorkerRecoveryOutcome.OwnershipMismatch
            currentAttempt = null
            lastClosedAttempt = null
            pendingShutdown = null
            ownerWorkers = null
            recoveryClaim = null
            state = OutboundLifecycleState.Stopped
            WorkerRecoveryOutcome.Recovered
        }
    }

    private suspend fun ownsRecoveryClaim(claim: WorkerRecoveryClaim, workers: OwnerWorkers): Boolean =
        gate.withLock { ownsRecoveryClaimLocked(claim, workers) }

    private fun ownsRecoveryClaimLocked(claim: WorkerRecoveryClaim, workers: OwnerWorkers): Boolean =
        recoveryClaim == claim && ownerWorkers === workers &&
            state == OutboundLifecycleState.Fenced(claim.lifecycle, LifecycleFenceCause.WorkerExited(claim.fence))

    private suspend fun clearRecoveryClaim(claim: WorkerRecoveryClaim) {
        gate.withLock { if (recoveryClaim == claim) recoveryClaim = null }
    }

    suspend fun awaitStartupTerminalDrain(ownerBareJid: String) =
        finalizationOperations.awaitStartupTerminalDrain(requireNotNull(ownerWorkers), ownerBareJid)

    suspend fun submitTerminal(
        ownerBareJid: String,
        clientStanzaId: String,
        attempt: DeliveryAttemptRef,
        kind: DeliveryTerminalKind,
    ): TerminalCommandOutcome = finalizationOperations.submitTerminal(
        requireNotNull(ownerWorkers),
        ownerBareJid,
        clientStanzaId,
        attempt,
        kind,
    )

    fun signalDrain(attempt: DeliveryAttemptRef) =
        finalizationOperations.signalDrain(ownerWorkers, active(), attempt)

    private suspend fun stopCurrentOwner(
        lifecycle: SessionLifecycleRef,
        record: AttemptRecord?,
    ): LifecycleShutdownOutcome.FencedWithPending? {
        if (record != null) {
            val claim = claimDisconnect(record.handle)
                ?: return pendingLifecycleShutdown(
                    lifecycle,
                    LifecyclePendingComponent.NATIVE_DISCONNECT,
                    1,
                )
            when (
                val transport =
                    finalizationOperations.quiesceTransport(record, claim)
            ) {
                OwnerFinalizationResult.Finalized -> Unit
                is OwnerFinalizationResult.Pending ->
                    return pendingLifecycleShutdown(
                        lifecycle,
                        transport.component,
                        transport.count,
                    )
            }
        }
        if (!awaitLeaseDrain()) {
            return pendingLifecycleShutdown(
                lifecycle,
                LifecyclePendingComponent.ATTEMPT_LEASES,
                pendingLeaseCount(),
            )
        }
        val workers = ownerWorkers
            ?: return pendingLifecycleShutdown(lifecycle, LifecyclePendingComponent.OUTBOUND_DRAIN, 1)
        return when (val result = finalizationOperations.finalizeOwner(workers, lifecycle, record)) {
            OwnerFinalizationResult.Finalized -> null
            is OwnerFinalizationResult.Pending ->
                pendingLifecycleShutdown(lifecycle, result.component, result.count)
        }
    }

    private suspend fun compensateActivation(
        lifecycle: SessionLifecycleRef,
        handle: ConnectionAttemptHandle,
        knownAttempt: DeliveryAttemptRef?,
    ): Boolean = withContext(NonCancellable) {
        val shutdownOwnsTransition = gate.withLock {
            state is OutboundLifecycleState.Closing
        }
        transitionToClosing(lifecycle, handle, knownAttempt)
        val completed =
            finalizationOperations.compensateActivation(
                requireNotNull(ownerWorkers),
                lifecycle,
                handle,
                knownAttempt,
            )
        if (completed) {
            gate.withLock {
                currentAttempt = null
                lastClosedAttempt = null
                state = if (shutdownOwnsTransition) {
                    OutboundLifecycleState.Closing(lifecycle, null, null)
                } else {
                    OutboundLifecycleState.Open(lifecycle)
                }
            }
        }
        completed
    }

    private suspend fun compensateHandoff(
        lifecycle: SessionLifecycleRef,
        handle: ConnectionAttemptHandle,
        transition: DeliveryAttemptTransition,
    ) = withContext(NonCancellable) {
        transitionToClosing(lifecycle, handle, transition.fresh)
        val completed =
            finalizationOperations.compensateHandoff(
                requireNotNull(ownerWorkers),
                lifecycle,
                handle,
                transition,
            )
        if (!completed) {
            throw LifecycleTransitionException(
                lifecycle,
                LifecyclePendingComponent.ACTIVATION_COMPENSATION,
                1,
            )
        }
    }

    private suspend fun transitionToClosing(
        lifecycle: SessionLifecycleRef,
        handle: ConnectionAttemptHandle?,
        attempt: DeliveryAttemptRef?,
    ) {
        gate.withLock {
            state = OutboundLifecycleState.Closing(lifecycle, handle, attempt)
        }
    }

    private suspend fun awaitLeaseDrain(): Boolean {
        val waiter = gate.withLock {
            retainedOperations?.waiterIfRetained() ?: return true
        }
        return withTimeoutOrNull(transitionTimeoutMillis) {
            waiter.await()
            true
        } == true
    }

    private suspend fun awaitOtherOperations(
        excluded: OperationCapability,
    ): Boolean = withTimeoutOrNull(transitionTimeoutMillis) {
        while (true) {
            val waiter = gate.withLock {
                retainedOperations?.waiterIfOtherRetained(excluded)
            }
            if (waiter == null) return@withTimeoutOrNull true
            waiter.await()
        }
    } == true

    private suspend fun pendingLeaseCount(): Int =
        gate.withLock { retainedOperations?.retainedCount()?.coerceAtLeast(1) ?: 1 }

    private fun retainReservation(reservation: AdmissionReservation) {
        check(
            retainedOperations?.retain(
                OperationCapability(
                    lifecycle = reservation.lifecycle,
                    attempt = reservation.attempt,
                    operationId = reservation.token,
                ),
            ) == true,
        ) { "outbound lifecycle lost operation registry" }
    }

    private suspend fun releaseRotationMutation(lease: RotationMutationLease) {
        gate.withLock {
            if (rotationMutationLease != lease) return@withLock
            rotationMutationLease = null
            retainedOperations?.release(lease.capability)
        }
    }

    private companion object {
        const val TRANSITION_TIMEOUT_MILLIS = 5_000L
    }
}
