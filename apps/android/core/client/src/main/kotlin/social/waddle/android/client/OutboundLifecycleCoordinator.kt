package social.waddle.android.client

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CancellationException
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
    ownerFinalizer: (suspend (OwnerWorkers, SessionLifecycleRef, AttemptRecord?) -> OwnerFinalizationResult)? = null,
    durableRecoveryCleanup: DurableRecoveryCleanup? = null,
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
        durableRecoveryCleanup ?: ProductionDurableRecoveryCleanup(journal, resume, activeSession),
    )
    private val ownerFinalizer = ownerFinalizer ?: finalizationOperations::finalizeOwner
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
    ): LifecycleStartResult {
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
        try {
            finalizationOperations.startWorkers(
                scope = scope,
                workers = workers,
                onReady = ::onWorkerReady,
                onExit = ::onWorkerExit,
            )
            workers.terminal.awaitReady()
            workers.drain.awaitReady()
            val opened = gate.withLock {
                if (state == OutboundLifecycleState.Bootstrapping(lifecycle) && workers.bothReady()) {
                    state = OutboundLifecycleState.Open(lifecycle)
                    true
                } else {
                    false
                }
            }
            return if (opened) LifecycleStartResult.Started(lifecycle) else {
                phaseObserver.after(OutboundLifecyclePhase.STARTUP_READINESS_LOST)
                compensateFailedStart(lifecycle)
                LifecycleStartResult.Failed(lifecycle, LifecycleStartFailure.WORKER_READINESS_FAILED)
            }
        } catch (cancelled: CancellationException) {
            compensateFailedStart(lifecycle)
            return LifecycleStartResult.Failed(lifecycle, LifecycleStartFailure.CANCELLED)
        }
    }

    private suspend fun compensateFailedStart(lifecycle: SessionLifecycleRef) = withContext(NonCancellable) {
        val workers = gate.withLock { ownerWorkers?.takeIf { it.lifecycle == lifecycle } }
        if (workers?.isInstalled() == true) {
            workers.terminal.requestStop()
            workers.drain.requestStop()
            workers.terminal.awaitExit(transitionTimeoutMillis)
            workers.drain.awaitExit(transitionTimeoutMillis)
        }
        gate.withLock {
            if (state.lifecycleOrNull() == lifecycle && state !is OutboundLifecycleState.Fenced) {
                retainedOperations?.closeAdmissions()
                ownerWorkers = null
                retainedOperations = null
                currentAttempt = null
                lastClosedAttempt = null
                state = OutboundLifecycleState.Stopped
                workers?.let(::discardWorkerEvidence)
            }
        }
    }

    private suspend fun onWorkerReady(ownership: WorkerOwnership) {
        val accepted = gate.withLock {
            val workers = ownerWorkers ?: return@withLock false
            workers.lifecycle == ownership.lifecycle && workers.markReady(ownership)
        }
        if (accepted) phaseObserver.after(
            if (ownership.kind == WorkerKind.DELIVERY_TERMINAL) {
                OutboundLifecyclePhase.TERMINAL_WORKER_READY
            } else {
                OutboundLifecyclePhase.DRAIN_WORKER_READY
            },
        )
    }

    /** Callback completes only after this exact exit has closed future admission. */
    private suspend fun onWorkerExit(exit: WorkerExit) {
        gate.withLock {
            val workers = ownerWorkers
            if (workers == null) {
                WorkerExitExceptionEvidence.discard(exit.ownership())
                return@withLock
            }
            val decision = decideWorkerExitGate(
                state = state,
                exactOwner = workers.lifecycle == exit.lifecycle && workers.owns(exit.ownership()),
                firstExactExit = workers.recordExactExit(exit),
                exit = exit,
            )
            if (decision is WorkerExitGateDecision.Fence) {
                retainedOperations?.closeAdmissions()
                state = OutboundLifecycleState.Fenced(
                    exit.lifecycle,
                    decision.cause,
                )
            } else {
                WorkerExitExceptionEvidence.discard(exit.ownership())
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
        } catch (failure: Exception) {
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
        val capability = retainedOperations?.issue(active.attempt) ?: return@withLock null
        TransportConstructionClaim.issue(handle, capability)
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
        requireLifecycleRelease(
            retainedOperations?.release(claim.capability) ?: LifecycleReleaseOutcome.NotOwned,
            claim.capability,
            LifecycleReleaseSite.TRANSPORT_ATTACH,
        )
        record.client = client
        record.requiresClientCloseProof = true
        TransportAttachOutcome.Attached
    }

    /**
     * Complete the retained construction only after its unowned client closes.
     * The exact same claim may retry after a close/finally race; no other
     * release site admits an AlreadyReleased outcome.
     */
    suspend fun finishSupersededConstruction(claim: TransportConstructionClaim) {
        val outcome = gate.withLock {
            retainedOperations?.release(claim.capability) ?: LifecycleReleaseOutcome.NotOwned
        }
        requireLifecycleRelease(
            outcome,
            claim.capability,
            LifecycleReleaseSite.TRANSPORT_SUPERSEDED,
        )
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

    suspend fun beginShutdown(lifecycle: SessionLifecycleRef): BeginShutdownDecision =
        gate.withLock {
            val actual = state.lifecycleOrNull()
            if (actual != lifecycle) return@withLock BeginShutdownDecision.Stale(lifecycle, actual)
            if (state is OutboundLifecycleState.Fenced) {
                val fenced = state as OutboundLifecycleState.Fenced
                return@withLock BeginShutdownDecision.WorkerFenced(fenced.lifecycle, fenced.cause)
            }
            if (state is OutboundLifecycleState.Closing) {
                return@withLock BeginShutdownDecision.AlreadyClosing(lifecycle)
            }
            retainedOperations?.closeAdmissions()
            val record = currentAttempt
            state = OutboundLifecycleState.Closing(
                lifecycle = lifecycle,
                handle = record?.handle,
                attempt = record?.attempt ?: state.attemptOrNull(),
            )
            BeginShutdownDecision.Begun(lifecycle)
        }

    suspend fun acquireOutbound(
        source: DeliverySource,
        expectedOwnerBareJid: String? = null,
    ): OutboundAdmissionResult {
        val claim = claimOutboundReservation(expectedOwnerBareJid)
        val reservation = when (claim) {
            is RetainedOutboundReservationClaim.Granted -> claim.reservation
            RetainedOutboundReservationClaim.OwnerMismatch ->
                return OutboundAdmissionResult.OwnerMismatch
            RetainedOutboundReservationClaim.LifecycleUnavailable ->
                return OutboundAdmissionResult.LifecycleUnavailable
        }
        return materializeOutboundAdmission(activeSession, reservation, source)
    }

    private suspend fun claimOutboundReservation(
        expectedOwnerBareJid: String?,
    ): RetainedOutboundReservationClaim = gate.withLock {
        val claim = classifyOutboundReservation(state, expectedOwnerBareJid)
        when (claim) {
            OutboundReservationClaim.OwnerMismatch ->
                return@withLock RetainedOutboundReservationClaim.OwnerMismatch
            OutboundReservationClaim.LifecycleUnavailable ->
                return@withLock RetainedOutboundReservationClaim.LifecycleUnavailable
            is OutboundReservationClaim.Granted -> Unit
        }
        val reservation = retainReservation(claim.reservation)
            ?: return@withLock RetainedOutboundReservationClaim.LifecycleUnavailable
        RetainedOutboundReservationClaim.Granted(reservation)
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
            val outcome = releaseReservation(reservation.capability)
            requireLifecycleRelease(
                outcome,
                reservation.capability,
                LifecycleReleaseSite.DRAIN_CLIENT_UNAVAILABLE,
            )
            return null
        }
        return OutboundAdmissionLease.LiveOutbound.issue(
            client = client,
            purpose = LiveOutboundPurpose.Drain,
            capability = reservation.capability,
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
        return OutboundAdmissionLease.Terminal.issue(
            capability = reservation.capability,
        )
    }

    private suspend fun reserveAdmission(
        expectedOwnerBareJid: String?,
        expectedAttempt: DeliveryAttemptRef?,
        requireActive: Boolean,
    ): RetainedAdmission? = gate.withLock {
        val reservation = createAdmissionCandidate(
            state,
            expectedOwnerBareJid,
            expectedAttempt,
            requireActive,
        ) ?: return@withLock null
        retainReservation(reservation)
    }

    suspend fun releaseAdmission(lease: OutboundAdmissionLease): LifecycleReleaseOutcome =
        releaseReservation(lease.capability)

    private suspend fun releaseReservation(
        capability: LifecycleOperationRegistry.Lease,
    ): LifecycleReleaseOutcome =
        gate.withLock {
            retainedOperations?.release(capability)
                ?: LifecycleReleaseOutcome.NotOwned
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
            val capability = retainedOperations?.issue(transition.old)
            check(capability != null) {
                "outbound lifecycle lost operation registry"
            }
            val acquired = RotationMutationLease.issue(
                handle = handle,
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
            releaseRotationMutation(lease, failure)
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
                            rotationMutationLease === lease
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
                    rotationMutationLease === lease,
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
            if (state is OutboundLifecycleState.Fenced) {
                val fenced = state as OutboundLifecycleState.Fenced
                return LifecycleShutdownOutcome.WorkerFenced(
                    fenced.lifecycle,
                    fenced.cause,
                )
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
        phaseObserver.after(OutboundLifecyclePhase.SHUTDOWN_OWNER_FINALIZED)
        if (shutdown != null) {
            var awaitingInstalled = false
            val outcome = gate.withLock {
                val fenced = state as? OutboundLifecycleState.Fenced
                if (fenced != null) {
                    return@withLock LifecycleShutdownOutcome.WorkerFenced(fenced.lifecycle, fenced.cause)
                }
                if (state.lifecycleOrNull() != lifecycle) {
                    return@withLock LifecycleShutdownOutcome.Stale
                }
                pendingShutdown = shutdown
                val workers = ownerWorkers
                val awaiting = when (shutdown.component) {
                    LifecyclePendingComponent.TERMINAL_DRAIN -> workers?.terminalOwnership
                    LifecyclePendingComponent.OUTBOUND_DRAIN -> workers?.drainOwnership
                    else -> null
                }
                if (awaiting != null) {
                    retainedOperations?.closeAdmissions()
                    val recorded = workers?.exitFor(awaiting)
                    if (recorded != null) {
                        val cause = LifecycleFenceCause.WorkerExited(WorkerFence(recorded))
                        state = OutboundLifecycleState.Fenced(lifecycle, cause)
                        return@withLock LifecycleShutdownOutcome.WorkerFenced(lifecycle, cause)
                    }
                    state = OutboundLifecycleState.Fenced(
                        lifecycle,
                        LifecycleFenceCause.AwaitingRequestedWorkerExit(awaiting),
                    )
                    awaitingInstalled = true
                }
                shutdown
            }
            if (awaitingInstalled) {
                phaseObserver.after(OutboundLifecyclePhase.AWAITING_REQUESTED_WORKER_EXIT_INSTALLED)
                return gate.withLock {
                    val fenced = state as? OutboundLifecycleState.Fenced
                    if (fenced?.lifecycle == lifecycle) {
                        LifecycleShutdownOutcome.WorkerFenced(fenced.lifecycle, fenced.cause)
                    } else {
                        outcome
                    }
                }
            }
            return outcome
        }

        return gate.withLock {
            val fenced = state as? OutboundLifecycleState.Fenced
            if (fenced != null) {
                return@withLock LifecycleShutdownOutcome.WorkerFenced(fenced.lifecycle, fenced.cause)
            }
            if (state.lifecycleOrNull() != lifecycle) {
                return@withLock LifecycleShutdownOutcome.Stale
            }
            currentAttempt = null
            lastClosedAttempt = null
            pendingShutdown = null
            val clearedWorkers = ownerWorkers
            ownerWorkers = null
            state = OutboundLifecycleState.Stopped
            clearedWorkers?.let(::discardWorkerEvidence)
            LifecycleShutdownOutcome.Stopped
        }
    }

    suspend fun recoverFencedWorkers(lifecycle: SessionLifecycleRef): WorkerRecoveryOutcome {
        val decision = gate.withLock {
            val fenced = state as? OutboundLifecycleState.Fenced
                ?: return@withLock WorkerRecoveryClaimDecision.NotFenced
            if (fenced.lifecycle != lifecycle) {
                return@withLock WorkerRecoveryClaimDecision.OwnershipMismatch(lifecycle)
            }
            val awaiting = fenced.cause as? LifecycleFenceCause.AwaitingRequestedWorkerExit
            if (awaiting != null) {
                return@withLock WorkerRecoveryClaimDecision.AwaitingExit(lifecycle, awaiting.ownership)
            }
            val cause = fenced.cause as? LifecycleFenceCause.WorkerExited
                ?: return@withLock WorkerRecoveryClaimDecision.NotFenced
            val workers = ownerWorkers
                ?: return@withLock WorkerRecoveryClaimDecision.OwnershipMismatch(lifecycle)
            if (!workers.owns(cause.fence.exit.ownership())) {
                return@withLock WorkerRecoveryClaimDecision.OwnershipMismatch(lifecycle)
            }
            recoveryClaim?.let { return@withLock WorkerRecoveryClaimDecision.RecoveryInProgress(it) }
            retainedOperations?.closeAdmissions()
            val claim = WorkerRecoveryClaim(lifecycle, cause.fence, WorkerRecoveryToken.random()).also {
                recoveryClaim = it
            }
            WorkerRecoveryClaimDecision.Granted(claim, workers)
        }
        val granted = when (decision) {
            WorkerRecoveryClaimDecision.NotFenced -> return WorkerRecoveryOutcome.NotFenced
            is WorkerRecoveryClaimDecision.OwnershipMismatch ->
                return WorkerRecoveryOutcome.OwnershipMismatch(decision.lifecycle, state.lifecycleOrNull())
            is WorkerRecoveryClaimDecision.RecoveryInProgress ->
                return WorkerRecoveryOutcome.RecoveryInProgress(decision.claim)
            is WorkerRecoveryClaimDecision.AwaitingExit ->
                return WorkerRecoveryOutcome.WorkerExitPending(decision.lifecycle, decision.ownership)
            is WorkerRecoveryClaimDecision.Granted -> decision
        }
        val claim = granted.claim
        val workers = granted.workers

        try {
            val siblingDecision = gate.withLock {
                if (!ownsRecoveryClaimLocked(claim, workers)) {
                    RecoverySiblingStopDecision.RecoveryClaimLost
                } else {
                    decideRecoverySiblingStop(
                        failed = claim.fence.exit.ownership(),
                        terminal = workers.terminalOwnership,
                        terminalExit = workers.exitFor(workers.terminalOwnership),
                        drain = workers.drainOwnership,
                        drainExit = workers.exitFor(workers.drainOwnership),
                    )
                }
            }
            when (siblingDecision) {
                is RecoverySiblingStopDecision.Stop -> when (siblingDecision.sibling.kind) {
                    WorkerKind.DELIVERY_TERMINAL -> workers.terminal.requestStop()
                    WorkerKind.OUTBOUND_DRAIN -> workers.drain.requestStop()
                }
                RecoverySiblingStopDecision.AlreadyExited -> Unit
                RecoverySiblingStopDecision.RecoveryClaimLost,
                RecoverySiblingStopDecision.UnknownFailedWorker,
                is RecoverySiblingStopDecision.RecordedExitMismatch,
                -> return WorkerRecoveryOutcome.OwnershipMismatch(lifecycle, state.lifecycleOrNull())
            }
            val exits = listOf(
                workers.terminalOwnership to workers.terminal.awaitExit(transitionTimeoutMillis),
                workers.drainOwnership to workers.drain.awaitExit(transitionTimeoutMillis),
            )
            val timedOut = exits.firstOrNull { (_, outcome) -> outcome is WorkerAwaitOutcome.TimedOut }
            if (timedOut != null) {
                return WorkerRecoveryOutcome.WorkerExitPending(lifecycle, timedOut.first)
            }
            try {
                when (val receiptCleanup = workers.terminal.recoverUnresolvedReceiptCleanup()) {
                    TerminalReceiptRecoveryCleanupResult.NoPendingLease,
                    TerminalReceiptRecoveryCleanupResult.Released,
                    -> Unit
                    is TerminalReceiptRecoveryCleanupResult.Unresolved -> {
                        return WorkerRecoveryOutcome.TerminalReceiptCleanupFailed(
                            lifecycle = lifecycle,
                            claim = claim,
                            cleanup = receiptCleanup.evidence,
                        )
                    }
                }
            } catch (failure: TerminalReceiptCleanupException) {
                return WorkerRecoveryOutcome.TerminalReceiptCleanupFailed(
                    lifecycle = lifecycle,
                    claim = claim,
                    cleanup = failure.evidence,
                )
            }
            if (!awaitLeaseDrain()) {
                return WorkerRecoveryOutcome.RetainedOperationsPending(lifecycle, pendingLeaseCount())
            }
            if (!ownsRecoveryClaim(claim, workers)) return WorkerRecoveryOutcome.OwnershipMismatch(lifecycle, state.lifecycleOrNull())
            val cleanup = finalizationOperations.recoverDurableState(workers, lifecycle)
            when (cleanup) {
                OwnerFinalizationResult.Finalized -> Unit
                is OwnerFinalizationResult.Pending -> {
                    return WorkerRecoveryOutcome.DurableCleanupPending(
                        lifecycle = lifecycle,
                        claim = claim,
                        component = cleanup.component,
                        count = cleanup.count,
                        operation = cleanup.operation,
                        attempt = cleanup.attempt,
                    )
                }
                is OwnerFinalizationResult.DurableCleanupFailed -> {
                    return WorkerRecoveryOutcome.DurableCleanupFailed(
                        lifecycle = lifecycle,
                        claim = claim,
                        component = cleanup.component,
                        count = cleanup.count,
                        operation = cleanup.operation,
                        cause = cleanup.cause,
                        attempt = cleanup.attempt,
                    )
                }
            }
            return gate.withLock {
                if (!ownsRecoveryClaimLocked(claim, workers)) return@withLock WorkerRecoveryOutcome.OwnershipMismatch(lifecycle, state.lifecycleOrNull())
                currentAttempt = null
                lastClosedAttempt = null
                pendingShutdown = null
                ownerWorkers = null
                recoveryClaim = null
                state = OutboundLifecycleState.Stopped
                discardWorkerEvidence(workers)
                WorkerRecoveryOutcome.Recovered
            }
        } finally {
            clearRecoveryClaim(claim)
        }
    }

    private suspend fun ownsRecoveryClaim(claim: WorkerRecoveryClaim, workers: OwnerWorkers): Boolean =
        gate.withLock { ownsRecoveryClaimLocked(claim, workers) }

    private fun ownsRecoveryClaimLocked(claim: WorkerRecoveryClaim, workers: OwnerWorkers): Boolean =
        recoveryClaim === claim && ownerWorkers === workers &&
            state == OutboundLifecycleState.Fenced(claim.lifecycle, LifecycleFenceCause.WorkerExited(claim.fence))

    private suspend fun clearRecoveryClaim(claim: WorkerRecoveryClaim) {
        gate.withLock { if (recoveryClaim === claim) recoveryClaim = null }
    }

    private fun discardWorkerEvidence(workers: OwnerWorkers) {
        WorkerExitExceptionEvidence.discard(workers.terminalOwnership)
        WorkerExitExceptionEvidence.discard(workers.drainOwnership)
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

    fun signalDrain(attempt: DeliveryAttemptRef): DrainSignalOutcome =
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
                is OwnerFinalizationResult.DurableCleanupFailed ->
                    error("durable cleanup failure is only valid during recovery: $transport")
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
        return when (val result = ownerFinalizer(workers, lifecycle, record)) {
            OwnerFinalizationResult.Finalized -> null
            is OwnerFinalizationResult.Pending ->
                pendingLifecycleShutdown(lifecycle, result.component, result.count)
            is OwnerFinalizationResult.DurableCleanupFailed ->
                error("durable cleanup failure is only valid during recovery: $result")
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
        excluded: LifecycleOperationRegistry.Lease,
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
        gate.withLock { retainedOperations?.retainedCount() ?: 0 }

    private fun retainReservation(reservation: AdmissionCandidate): RetainedAdmission? {
        val capability = retainedOperations?.issue(reservation.attempt) ?: return null
        return RetainedAdmission.issue(capability)
    }

    private suspend fun releaseRotationMutation(
        lease: RotationMutationLease,
        primary: Throwable? = null,
    ) {
        val outcome = gate.withLock {
            when (val decision = decideRotationMutationRelease(rotationMutationLease, lease.capability)) {
                is RotationMutationReleaseDecision.NotOwned -> {
                    check(rotationMutationLease === decision.current)
                    LifecycleReleaseOutcome.NotOwned
                }
                is RotationMutationReleaseDecision.ReleaseCurrent -> {
                    check(rotationMutationLease === decision.current)
                    rotationMutationLease = null
                    retainedOperations?.release(lease.capability) ?: LifecycleReleaseOutcome.NotOwned
                }
            }
        }
        requireLifecycleRelease(
            outcome,
            lease.capability,
            LifecycleReleaseSite.ROTATION_MUTATION,
            primary = primary,
        )
    }

    private companion object {
        const val TRANSITION_TIMEOUT_MILLIS = 5_000L
    }
}
