package social.waddle.android.client

import kotlinx.coroutines.CompletableDeferred
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.client.ffi.WaddleClientInterface
import java.util.UUID

internal fun OutboundLifecycleState.lifecycleOrNull(): SessionLifecycleRef? =
    when (this) {
        is OutboundLifecycleState.Bootstrapping -> lifecycle
        is OutboundLifecycleState.Open -> lifecycle
        is OutboundLifecycleState.Active -> lifecycle
        is OutboundLifecycleState.Handoff -> lifecycle
        is OutboundLifecycleState.Closing -> lifecycle
        is OutboundLifecycleState.Fenced -> lifecycle
        OutboundLifecycleState.Stopped -> null
    }

internal fun OutboundLifecycleState.attemptOrNull(): DeliveryAttemptRef? =
    when (this) {
        is OutboundLifecycleState.Active -> attempt
        is OutboundLifecycleState.Handoff -> nextAttempt ?: previousAttempt
        is OutboundLifecycleState.Closing -> attempt
        is OutboundLifecycleState.Bootstrapping,
        is OutboundLifecycleState.Fenced,
        is OutboundLifecycleState.Open,
        OutboundLifecycleState.Stopped,
        -> null
    }

internal val LifecycleShutdownTarget.lifecycle: SessionLifecycleRef
    get() = when (this) {
        is LifecycleShutdownTarget.CurrentOwner -> lifecycle
        is LifecycleShutdownTarget.ExactAttempt -> lifecycle
    }

internal class AttemptRecord(
    val lifecycle: SessionLifecycleRef,
    val handle: ConnectionAttemptHandle,
    initialAttempt: DeliveryAttemptRef,
) {
    @Volatile
    var attempt: DeliveryAttemptRef = initialAttempt

    @Volatile
    var client: WaddleClientInterface? = null

    @Volatile
    var disconnectStarted: Boolean = false

    /**
     * Production construction records a close proof before its attempt can be
     * finalized. The legacy attach helper is retained for narrowly-scoped
     * tests that do not own a closeable transport.
     */
    @Volatile
    var requiresClientCloseProof: Boolean = false

    val disconnectResult = CompletableDeferred<Boolean>()
    val producerStopped = CompletableDeferred<Unit>()
    val clientClosed = CompletableDeferred<Boolean>()
}

/**
 * A coordinator creates one registry for one lifecycle only. Consequently a
 * late completion from a retired lifecycle cannot remove work retained by a
 * replacement owner, even when both owners have the same bare JID.
 *
 * All methods are called under OutboundLifecycleStateStore's short gate.
 */
internal class LifecycleOperationRegistry(
    private val lifecycle: SessionLifecycleRef,
) {
    /** Identity, not values, is the release authority. */
    internal interface Lease {
        val lifecycle: SessionLifecycleRef
        val attempt: DeliveryAttemptRef?
        val operationId: UUID
    }

    /** Only this registry can construct or mutate issued release authority. */
    private class IssuedLease(
        override val lifecycle: SessionLifecycleRef,
        override val attempt: DeliveryAttemptRef?,
        override val operationId: UUID,
        private val registry: LifecycleOperationRegistry,
    ) : Lease {
        private var released = false

        fun releaseFrom(owner: LifecycleOperationRegistry, active: IssuedLease?): LifecycleReleaseOutcome =
            when {
                registry !== owner -> LifecycleReleaseOutcome.NotOwned
                released -> LifecycleReleaseOutcome.AlreadyReleased
                active !== this -> LifecycleReleaseOutcome.NotOwned
                else -> {
                    released = true
                    LifecycleReleaseOutcome.Released
                }
            }

        fun isOwnedBy(owner: LifecycleOperationRegistry): Boolean = registry === owner && !released
    }

    private var admissionsOpen = true
    private val retained = mutableMapOf<UUID, IssuedLease>()
    private var emptyWaiter: CompletableDeferred<Unit>? = null
    private var retentionChanged: CompletableDeferred<Unit>? = null

    fun issue(attempt: DeliveryAttemptRef?, operationId: UUID = UUID.randomUUID()): Lease? {
        if (
            !admissionsOpen ||
            attempt?.ownerBareJid?.let { it != lifecycle.ownerBareJid } == true ||
            retained.containsKey(operationId)
        ) {
            return null
        }
        return IssuedLease(lifecycle, attempt, operationId, this).also { retained[operationId] = it }
    }

    fun release(lease: Lease): LifecycleReleaseOutcome {
        val issued = lease as? IssuedLease ?: return LifecycleReleaseOutcome.NotOwned
        val outcome = issued.releaseFrom(this, retained[issued.operationId])
        if (outcome != LifecycleReleaseOutcome.Released) return outcome
        retained.remove(issued.operationId)
        retentionChanged?.complete(Unit)
        retentionChanged = null
        if (retained.isEmpty()) {
            emptyWaiter?.complete(Unit)
            emptyWaiter = null
        }
        return LifecycleReleaseOutcome.Released
    }

    fun closeAdmissions() {
        admissionsOpen = false
    }

    fun owns(lease: Lease): Boolean =
        (lease as? IssuedLease)?.let { it.isOwnedBy(this) && retained[it.operationId] === it } == true

    fun waiterIfRetained(): CompletableDeferred<Unit>? =
        if (retained.isEmpty()) {
            null
        } else {
            emptyWaiter ?: CompletableDeferred<Unit>().also {
            emptyWaiter = it
        }
        }

    fun waiterIfOtherRetained(
        excluded: Lease,
    ): CompletableDeferred<Unit>? =
        if (retained.values.none { it !== excluded }) {
            null
        } else {
            retentionChanged ?: CompletableDeferred<Unit>().also {
                retentionChanged = it
            }
        }

    fun retainedCount(): Int = retained.size
}

internal enum class LifecycleReleaseOutcome {
    Released,
    AlreadyReleased,
    NotOwned,
}

internal enum class LifecycleReleaseSite {
    OFFLINE_OUTBOUND,
    LIVE_OUTBOUND,
    TERMINAL_COMMAND,
    OUTBOUND_DRAIN,
    DRAIN_CLIENT_UNAVAILABLE,
    TRANSPORT_ATTACH,
    TRANSPORT_SUPERSEDED,
    ROTATION_MUTATION,
}

internal class LifecycleReleaseViolation(
    val outcome: LifecycleReleaseOutcome,
    val site: LifecycleReleaseSite,
    val lifecycle: SessionLifecycleRef,
    val attempt: DeliveryAttemptRef?,
    val operationId: UUID,
) : IllegalStateException(
    "lifecycle release violation outcome=$outcome site=$site operation=$operationId",
)

internal fun requireLifecycleRelease(
    outcome: LifecycleReleaseOutcome,
    capability: LifecycleOperationRegistry.Lease,
    site: LifecycleReleaseSite,
    primary: Throwable? = null,
) {
    when (outcome) {
        LifecycleReleaseOutcome.Released -> Unit
        LifecycleReleaseOutcome.AlreadyReleased ->
            if (site == LifecycleReleaseSite.TRANSPORT_SUPERSEDED) {
                Unit
            } else {
                surfaceReleaseViolation(outcome, capability, site, primary)
            }
        LifecycleReleaseOutcome.NotOwned ->
            surfaceReleaseViolation(outcome, capability, site, primary)
    }
}

private fun surfaceReleaseViolation(
    outcome: LifecycleReleaseOutcome,
    capability: LifecycleOperationRegistry.Lease,
    site: LifecycleReleaseSite,
    primary: Throwable?,
) {
    val violation = LifecycleReleaseViolation(
        outcome = outcome,
        site = site,
        lifecycle = capability.lifecycle,
        attempt = capability.attempt,
        operationId = capability.operationId,
    )
    if (primary == null) throw violation
    primary.addSuppressed(violation)
}

internal class TransportConstructionClaim private constructor(
    val handle: ConnectionAttemptHandle,
    val capability: LifecycleOperationRegistry.Lease,
) {
    val lifecycle: SessionLifecycleRef get() = capability.lifecycle
    val attempt: DeliveryAttemptRef get() = requireNotNull(capability.attempt)

    companion object {
        internal fun issue(
            handle: ConnectionAttemptHandle,
            capability: LifecycleOperationRegistry.Lease,
        ): TransportConstructionClaim {
            requireNotNull(capability.attempt) { "construction capability requires an attempt" }
            return TransportConstructionClaim(handle, capability)
        }
    }
}

/** Owns the complete durable RESUME-to-fresh mutation, including retries. */
internal class RotationMutationLease private constructor(
    val handle: ConnectionAttemptHandle,
    val fresh: DeliveryAttemptRef,
    val capability: LifecycleOperationRegistry.Lease,
) {
    val lifecycle: SessionLifecycleRef get() = capability.lifecycle
    val old: DeliveryAttemptRef get() = requireNotNull(capability.attempt)

    companion object {
        internal fun issue(
            handle: ConnectionAttemptHandle,
            fresh: DeliveryAttemptRef,
            capability: LifecycleOperationRegistry.Lease,
        ): RotationMutationLease {
            val old = requireNotNull(capability.attempt) { "rotation capability requires an attempt" }
            require(old.ownerBareJid == fresh.ownerBareJid) { "rotation capability owner mismatch" }
            return RotationMutationLease(handle, fresh, capability)
        }
    }
}

/** A stale rotation finalizer must never consume the current mutation lease. */
internal sealed interface RotationMutationReleaseDecision {
    data class ReleaseCurrent(val current: RotationMutationLease) : RotationMutationReleaseDecision
    data class NotOwned(val current: RotationMutationLease?) : RotationMutationReleaseDecision
}

internal fun decideRotationMutationRelease(
    current: RotationMutationLease?,
    requested: LifecycleOperationRegistry.Lease,
): RotationMutationReleaseDecision =
    if (current?.capability === requested) {
        RotationMutationReleaseDecision.ReleaseCurrent(current)
    } else {
        RotationMutationReleaseDecision.NotOwned(current)
    }

internal sealed interface TransportAttachOutcome {
    data object Attached : TransportAttachOutcome

    /** The caller still owns and must close the just-constructed client. */
    data object SupersededAndClose : TransportAttachOutcome
}

internal data class AdmissionCandidate(
    val lifecycle: SessionLifecycleRef,
    val attempt: DeliveryAttemptRef?,
)

/** A retained admission derives its owner identity from its opaque capability. */
internal class RetainedAdmission private constructor(
    val capability: LifecycleOperationRegistry.Lease,
) {
    val lifecycle: SessionLifecycleRef get() = capability.lifecycle
    val attempt: DeliveryAttemptRef? get() = capability.attempt

    companion object {
        internal fun issue(capability: LifecycleOperationRegistry.Lease): RetainedAdmission =
            RetainedAdmission(capability)
    }
}

internal sealed interface OutboundReservationClaim {
    data class Granted(
        val reservation: AdmissionCandidate,
    ) : OutboundReservationClaim

    data object OwnerMismatch : OutboundReservationClaim
    data object LifecycleUnavailable : OutboundReservationClaim
}

internal sealed interface RetainedOutboundReservationClaim {
    data class Granted(val reservation: RetainedAdmission) : RetainedOutboundReservationClaim
    data object OwnerMismatch : RetainedOutboundReservationClaim
    data object LifecycleUnavailable : RetainedOutboundReservationClaim
}

/** Pure, exact-worker selection for recovery; requestStop itself occurs after the gate. */
internal sealed interface RecoverySiblingStopDecision {
    data class Stop(val sibling: WorkerOwnership) : RecoverySiblingStopDecision
    data object AlreadyExited : RecoverySiblingStopDecision
    data object UnknownFailedWorker : RecoverySiblingStopDecision
    data object RecoveryClaimLost : RecoverySiblingStopDecision
    data class RecordedExitMismatch(
        val expected: WorkerOwnership,
        val actual: WorkerOwnership,
    ) : RecoverySiblingStopDecision
}

internal fun decideRecoverySiblingStop(
    failed: WorkerOwnership,
    terminal: WorkerOwnership,
    terminalExit: WorkerExit?,
    drain: WorkerOwnership,
    drainExit: WorkerExit?,
): RecoverySiblingStopDecision {
    terminalExit?.let { exit ->
        if (exit.ownership() != terminal) {
            return RecoverySiblingStopDecision.RecordedExitMismatch(terminal, exit.ownership())
        }
    }
    drainExit?.let { exit ->
        if (exit.ownership() != drain) {
            return RecoverySiblingStopDecision.RecordedExitMismatch(drain, exit.ownership())
        }
    }
    return when (failed) {
        terminal -> if (drainExit == null) {
            RecoverySiblingStopDecision.Stop(drain)
        } else {
            RecoverySiblingStopDecision.AlreadyExited
        }
        drain -> if (terminalExit == null) {
            RecoverySiblingStopDecision.Stop(terminal)
        } else {
            RecoverySiblingStopDecision.AlreadyExited
        }
        else -> RecoverySiblingStopDecision.UnknownFailedWorker
    }
}

internal sealed interface CloseClaim {
    data class Owned(val record: AttemptRecord) : CloseClaim
    data object AlreadyClosed : CloseClaim
    data object SessionShutdown : CloseClaim
    data object Stale : CloseClaim
}

internal data class CloseClaimDecision(
    val claim: CloseClaim,
    val nextState: OutboundLifecycleState? = null,
    val completeProducer: Boolean = false,
)

internal fun decideAttemptClose(
    state: OutboundLifecycleState,
    currentAttempt: AttemptRecord?,
    lastClosedAttempt: AttemptRecord?,
    handle: ConnectionAttemptHandle,
    producerQuiesced: Boolean,
): CloseClaimDecision {
    if (currentAttempt?.handle != handle) {
        val claim = if (lastClosedAttempt?.handle == handle) {
            CloseClaim.AlreadyClosed
        } else {
            CloseClaim.Stale
        }
        return CloseClaimDecision(claim)
    }
    if (state is OutboundLifecycleState.Closing) {
        return CloseClaimDecision(
            CloseClaim.SessionShutdown,
            completeProducer = producerQuiesced,
        )
    }
    if (state !is OutboundLifecycleState.Active) {
        return CloseClaimDecision(CloseClaim.Stale)
    }
    return CloseClaimDecision(
        claim = CloseClaim.Owned(currentAttempt),
        nextState = OutboundLifecycleState.Closing(
            lifecycle = currentAttempt.lifecycle,
            handle = currentAttempt.handle,
            attempt = currentAttempt.attempt,
        ),
    )
}

internal fun pendingLifecycleShutdown(
    lifecycle: SessionLifecycleRef,
    component: LifecyclePendingComponent,
    pending: Int,
) = LifecycleShutdownOutcome.FencedWithPending(
    lifecycle = lifecycle,
    component = component,
    pending = pending,
)

internal fun AttemptCloseOutcome.toShutdownOutcome(
    lifecycle: SessionLifecycleRef,
): LifecycleShutdownOutcome =
    when (this) {
        AttemptCloseOutcome.AlreadyClosed,
        AttemptCloseOutcome.Closed,
        -> LifecycleShutdownOutcome.AttemptClosed
        is AttemptCloseOutcome.FencedWithPending ->
            LifecycleShutdownOutcome.FencedWithPending(
                lifecycle,
                component,
                pending,
            )
        AttemptCloseOutcome.OwnedBySessionShutdown,
        AttemptCloseOutcome.Stale,
        -> LifecycleShutdownOutcome.Stale
    }

internal sealed interface DisconnectClaim {
    val result: CompletableDeferred<Boolean>

    data class Execute(
        val record: AttemptRecord,
        override val result: CompletableDeferred<Boolean>,
    ) : DisconnectClaim

    data class Wait(
        override val result: CompletableDeferred<Boolean>,
    ) : DisconnectClaim
}
