package social.waddle.android.client

import kotlinx.coroutines.CompletableDeferred
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
        drainWorker,
        journal,
        phaseObserver,
        resume,
    )
    private val finalizationOperations = OutboundLifecycleFinalizationOperations(
        activeSession,
        drainWorker,
        journal,
        resume,
        terminalWorker,
        transitionTimeoutMillis,
    )
    private val leases = mutableMapOf<UUID, SessionLifecycleRef>()
    private var leaseWaiter: CompletableDeferred<Unit>? = null
    private var currentAttempt: AttemptRecord? = null
    private var lastClosedAttempt: AttemptRecord? = null
    private var pendingShutdown: LifecycleShutdownOutcome.FencedWithPending? = null

    @Volatile
    private var state: OutboundLifecycleState = OutboundLifecycleState.Stopped

    suspend fun start(
        scope: CoroutineScope,
        ownerBareJid: String,
    ): SessionLifecycleRef {
        // Mint identity before the first suspension so replacement can never
        // inherit a partially-created generation.
        val lifecycle = SessionLifecycleRef.create(ownerBareJid)
        return gate.withLock {
            check(state == OutboundLifecycleState.Stopped) {
                "outbound lifecycle is not restartable from $state"
            }
            finalizationOperations.startWorkers(scope, lifecycle)
            currentAttempt = null
            lastClosedAttempt = null
            pendingShutdown = null
            state = OutboundLifecycleState.Open(lifecycle)
            lifecycle
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
            val active = phaseOperations.publishActivation(lifecycle, handle, bootstrap)
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
            state is OutboundLifecycleState.Closing
        ) {
            return@withLock false
        }
        record.client = client
        true
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
            leases[reservation.token] = reservation.lifecycle
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
            releaseReservation(reservation.token, reservation.lifecycle)
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
        leases[reservation.token] = reservation.lifecycle
        reservation
    }

    suspend fun releaseAdmission(lease: OutboundAdmissionLease) =
        releaseReservation(lease.token, lease.lifecycle)

    private suspend fun releaseReservation(
        token: UUID,
        lifecycle: SessionLifecycleRef,
    ) {
        gate.withLock {
            if (leases.remove(token) != lifecycle) return@withLock
            if (leases.isEmpty()) {
                leaseWaiter?.complete(Unit)
                leaseWaiter = null
            }
        }
    }

    suspend fun rotate(
        handle: ConnectionAttemptHandle,
        transition: DeliveryAttemptTransition,
        affectedStanzaIds: Set<String>,
    ): ResumeHandoffOutcome {
        val lifecycle = gate.withLock {
            val active = state as? OutboundLifecycleState.Active
            if (
                active?.handle != handle ||
                active.attempt != transition.old ||
                active.lifecycle.ownerBareJid != transition.old.ownerBareJid
            ) {
                return ResumeHandoffOutcome.Rejected
            }
            state = OutboundLifecycleState.Handoff(
                lifecycle = active.lifecycle,
                handle = handle,
                previousAttempt = transition.old,
                nextAttempt = transition.fresh,
            )
            active.lifecycle
        }
        return try {
            performRotation(
                lifecycle,
                handle,
                transition,
                affectedStanzaIds,
            )
        } catch (failure: Throwable) {
            val stillHandoff = gate.withLock {
                val handoff = state as? OutboundLifecycleState.Handoff
                handoff?.lifecycle == lifecycle && handoff.handle == handle
            }
            if (stillHandoff) compensateHandoff(lifecycle, handle, transition)
            throw failure
        }
    }

    private suspend fun performRotation(
        lifecycle: SessionLifecycleRef,
        handle: ConnectionAttemptHandle,
        transition: DeliveryAttemptTransition,
        affectedStanzaIds: Set<String>,
    ): ResumeHandoffOutcome {
        if (!awaitLeaseDrain()) {
            transitionToClosing(lifecycle, handle, transition.old)
            throw LifecycleTransitionException(
                lifecycle,
                LifecyclePendingComponent.ATTEMPT_LEASES,
                pendingLeaseCount(),
            )
        }

        val journalOutcome =
            phaseOperations.journalRotation(transition, affectedStanzaIds)
        val smVersion = when (journalOutcome) {
            is RotationJournalOutcome.Accepted -> journalOutcome.smVersion
            RotationJournalOutcome.Rejected -> {
                gate.withLock {
                    val handoff = state as? OutboundLifecycleState.Handoff
                    if (handoff?.lifecycle == lifecycle && handoff.handle == handle) {
                        state = OutboundLifecycleState.Active(
                            lifecycle,
                            handle,
                            transition.old,
                        )
                    }
                }
                return ResumeHandoffOutcome.Rejected
            }
        }

        if (!phaseOperations.publishRotation(lifecycle, handle, transition, smVersion)) {
            compensateHandoff(lifecycle, handle, transition)
            return ResumeHandoffOutcome.Rejected
        }
        gate.withLock {
            val handoff = state as? OutboundLifecycleState.Handoff
            check(handoff?.lifecycle == lifecycle && handoff.handle == handle) {
                "resume handoff lost lifecycle authority"
            }
            currentAttempt?.attempt = transition.fresh
            state = OutboundLifecycleState.Active(lifecycle, handle, transition.fresh)
        }
        phaseOperations.rotationPublished()
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
        if (!awaitLeaseDrain()) {
            return AttemptCloseOutcome.FencedWithPending(
                LifecyclePendingComponent.ATTEMPT_LEASES,
                pendingLeaseCount(),
            )
        }
        val finalized = finalizationOperations.finalizeAttemptClose(record)
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
            }
            return shutdown
        }

        gate.withLock {
            currentAttempt = null
            lastClosedAttempt = null
            pendingShutdown = null
            state = OutboundLifecycleState.Stopped
        }
        return LifecycleShutdownOutcome.Stopped
    }

    /**
     * Explicit recovery gate for a terminal worker that stopped with durable
     * intents retained. Ordinary start remains forbidden while Closing; this
     * transition is allowed only after all transports/attempts are fenced and
     * the old worker is fully gone. The next lifecycle drains the journal
     * before admitting outbound replay.
     */
    suspend fun recoverFencedTerminal(
        lifecycle: SessionLifecycleRef,
    ): Boolean {
        val eligible = gate.withLock {
            isTerminalRecoveryEligible(
                state,
                pendingShutdown,
                lifecycle,
                leases.isEmpty(),
            )
        }
        if (!eligible) return false
        if (!finalizationOperations.terminalRecoveryReady(lifecycle.ownerBareJid)) {
            return false
        }
        return gate.withLock {
            val stillEligible = isTerminalRecoveryEligible(
                state,
                pendingShutdown,
                lifecycle,
                leases.isEmpty(),
            )
            if (!stillEligible) {
                return@withLock false
            }
            currentAttempt = null
            lastClosedAttempt = null
            pendingShutdown = null
            state = OutboundLifecycleState.Stopped
            true
        }
    }

    suspend fun awaitStartupTerminalDrain(ownerBareJid: String) =
        finalizationOperations.awaitStartupTerminalDrain(ownerBareJid)

    suspend fun submitTerminal(
        ownerBareJid: String,
        clientStanzaId: String,
        attempt: DeliveryAttemptRef,
        kind: DeliveryTerminalKind,
    ) = finalizationOperations.submitTerminal(ownerBareJid, clientStanzaId, attempt, kind)

    fun signalDrain(attempt: DeliveryAttemptRef) =
        finalizationOperations.signalDrain(active(), attempt)

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
        return when (
            val result = finalizationOperations.finalizeOwner(lifecycle, record)
        ) {
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
            finalizationOperations.compensateActivation(lifecycle, handle, knownAttempt)
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
            finalizationOperations.compensateHandoff(lifecycle, handle, transition)
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
            if (leases.isEmpty()) return true
            leaseWaiter ?: CompletableDeferred<Unit>().also { leaseWaiter = it }
        }
        return withTimeoutOrNull(transitionTimeoutMillis) {
            waiter.await()
            true
        } == true
    }

    private suspend fun pendingLeaseCount(): Int =
        gate.withLock { leases.size.coerceAtLeast(1) }

    private companion object {
        const val TRANSITION_TIMEOUT_MILLIS = 5_000L
    }
}
