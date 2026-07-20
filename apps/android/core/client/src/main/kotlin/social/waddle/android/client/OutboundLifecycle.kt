package social.waddle.android.client

import social.waddle.android.client.prefs.DeliveryAttemptRef
import java.util.UUID

@JvmInline
internal value class SessionLifecycleId private constructor(
    val value: UUID,
) {
    companion object {
        fun random(): SessionLifecycleId = SessionLifecycleId(UUID.randomUUID())
    }
}

internal data class SessionLifecycleRef(
    val ownerBareJid: String,
    val id: SessionLifecycleId,
) {
    companion object {
        fun create(ownerBareJid: String): SessionLifecycleRef =
            SessionLifecycleRef(ownerBareJid, SessionLifecycleId.random())
    }
}

/** Startup always exposes the installed lifecycle, including a recoverable failure. */
internal sealed interface LifecycleStartResult {
    val lifecycle: SessionLifecycleRef

    data class Started(override val lifecycle: SessionLifecycleRef) : LifecycleStartResult

    data class Failed(
        override val lifecycle: SessionLifecycleRef,
        val cause: LifecycleStartFailure,
    ) : LifecycleStartResult
}

internal enum class LifecycleStartFailure {
    WORKER_CONSTRUCTION_FAILED,
    WORKER_READINESS_FAILED,
    CANCELLED,
}

internal class LifecycleStartException(
    val result: LifecycleStartResult.Failed,
) : IllegalStateException("outbound worker startup failed: ${result.cause}")

internal sealed interface BeginShutdownDecision {
    data class Begun(val lifecycle: SessionLifecycleRef) : BeginShutdownDecision
    data class AlreadyClosing(val lifecycle: SessionLifecycleRef) : BeginShutdownDecision
    data class WorkerFenced(
        val lifecycle: SessionLifecycleRef,
        val cause: LifecycleFenceCause,
    ) : BeginShutdownDecision
    data class Stale(
        val requested: SessionLifecycleRef,
        val actual: SessionLifecycleRef?,
    ) : BeginShutdownDecision
}

internal enum class WorkerKind {
    OUTBOUND_DRAIN,
    DELIVERY_TERMINAL,
}

@JvmInline
internal value class WorkerGeneration private constructor(
    val value: UUID,
) {
    companion object {
        fun random(): WorkerGeneration = WorkerGeneration(UUID.randomUUID())
    }
}

internal data class WorkerOwnership(
    val lifecycle: SessionLifecycleRef,
    val kind: WorkerKind,
    val generation: WorkerGeneration,
)

/** Structured worker failure information deliberately excludes the Throwable. */
internal sealed interface WorkerFailureKind {
    data object DEPENDENCY_FAILURE : WorkerFailureKind

    data class TERMINAL_RECEIPT_APPLICATION(
        val failure: TerminalReceiptApplicationFailure,
    ) : WorkerFailureKind
}

internal sealed interface WorkerExitReason {
    data object RequestedStop : WorkerExitReason
    data object OwnerScopeCancelled : WorkerExitReason
    data object UnexpectedReturn : WorkerExitReason

    data class UnexpectedFailure(
        val kind: WorkerFailureKind,
    ) : WorkerExitReason
}

internal data class WorkerExit(
    val lifecycle: SessionLifecycleRef,
    val generation: WorkerGeneration,
    val kind: WorkerKind,
    val reason: WorkerExitReason,
)

internal sealed interface WorkerExitGateDecision {
    data object Ignore : WorkerExitGateDecision
    data object RecordOnly : WorkerExitGateDecision
    data class Fence(val cause: LifecycleFenceCause.WorkerExited) : WorkerExitGateDecision
}

/** Pure lifecycle transition: callback identity is validated before mutation. */
internal fun decideWorkerExitGate(
    state: OutboundLifecycleState,
    exactOwner: Boolean,
    firstExactExit: Boolean,
    exit: WorkerExit,
): WorkerExitGateDecision {
    if (!exactOwner || !firstExactExit) return WorkerExitGateDecision.Ignore
    val exited = LifecycleFenceCause.WorkerExited(WorkerFence(exit))
    val fenced = state as? OutboundLifecycleState.Fenced
    if (fenced != null) {
        val cause = fenced.cause
        if (cause is LifecycleFenceCause.WorkerExited) return WorkerExitGateDecision.Ignore
        val awaiting = cause as LifecycleFenceCause.AwaitingRequestedWorkerExit
        return if (
            exit.ownership() == awaiting.ownership ||
            exit.reason !is WorkerExitReason.RequestedStop
        ) {
            WorkerExitGateDecision.Fence(exited)
        } else {
            WorkerExitGateDecision.RecordOnly
        }
    }
    return if (state is OutboundLifecycleState.Closing && exit.reason is WorkerExitReason.RequestedStop) {
        WorkerExitGateDecision.RecordOnly
    } else {
        WorkerExitGateDecision.Fence(exited)
    }
}

internal sealed interface WorkerAwaitOutcome {
    data class Exited(
        val exit: WorkerExit,
    ) : WorkerAwaitOutcome

    data object TimedOut : WorkerAwaitOutcome
}

internal data class TerminalWorkerFailure(
    val kind: WorkerFailureKind,
)

internal sealed interface TerminalCommandOutcome {
    data object Committed : TerminalCommandOutcome
    data object WorkerUnavailable : TerminalCommandOutcome

    data class Failed(
        val failure: TerminalWorkerFailure,
    ) : TerminalCommandOutcome
}

internal sealed interface DrainSignalOutcome {
    data object Accepted : DrainSignalOutcome
    data object Mismatch : DrainSignalOutcome
    data object WorkerUnavailable : DrainSignalOutcome
}

internal class TerminalWorkerUnavailableException : IllegalStateException()

internal class TerminalWorkerCommandFailedException(
    val failure: TerminalWorkerFailure,
) : IllegalStateException()

internal fun requireTerminalCommitted(outcome: TerminalCommandOutcome) {
    when (outcome) {
        TerminalCommandOutcome.Committed -> Unit
        TerminalCommandOutcome.WorkerUnavailable -> throw TerminalWorkerUnavailableException()
        is TerminalCommandOutcome.Failed -> throw TerminalWorkerCommandFailedException(outcome.failure)
    }
}

internal data class WorkerFence(
    val exit: WorkerExit,
)

internal sealed interface LifecycleFenceCause {
    data class WorkerExited(val fence: WorkerFence) : LifecycleFenceCause
    data class AwaitingRequestedWorkerExit(val ownership: WorkerOwnership) : LifecycleFenceCause
}

@JvmInline
internal value class WorkerRecoveryToken private constructor(val value: UUID) {
    companion object {
        fun random(): WorkerRecoveryToken = WorkerRecoveryToken(UUID.randomUUID())
    }
}

internal data class WorkerRecoveryClaim(
    val lifecycle: SessionLifecycleRef,
    val fence: WorkerFence,
    val token: WorkerRecoveryToken,
)

internal sealed interface WorkerRecoveryOutcome {
    data object Recovered : WorkerRecoveryOutcome
    data object NotFenced : WorkerRecoveryOutcome
    data class OwnershipMismatch(
        val requested: SessionLifecycleRef,
        val actual: SessionLifecycleRef?,
    ) : WorkerRecoveryOutcome
    data class WorkerFenced(
        val lifecycle: SessionLifecycleRef,
        val cause: LifecycleFenceCause,
    ) : WorkerRecoveryOutcome
    data class RecoveryInProgress(val claim: WorkerRecoveryClaim) : WorkerRecoveryOutcome
    data class RetainedOperationsPending(
        val lifecycle: SessionLifecycleRef,
        val count: Int,
    ) : WorkerRecoveryOutcome
    data class WorkerExitPending(
        val lifecycle: SessionLifecycleRef,
        val ownership: WorkerOwnership,
    ) : WorkerRecoveryOutcome
    data class DurableCleanupPending(
        val lifecycle: SessionLifecycleRef,
        val claim: WorkerRecoveryClaim,
        val component: LifecyclePendingComponent,
        val count: Int,
        val operation: DurableCleanupOperation,
        val attempt: DeliveryAttemptRef?,
    ) : WorkerRecoveryOutcome
    data class DurableCleanupFailed(
        val lifecycle: SessionLifecycleRef,
        val claim: WorkerRecoveryClaim,
        val component: LifecyclePendingComponent,
        val count: Int,
        val operation: DurableCleanupOperation,
        val cause: DurableCleanupFailureCause,
        val attempt: DeliveryAttemptRef?,
    ) : WorkerRecoveryOutcome
    /**
     * The terminal worker still owns this exact receipt lease. Recovery must
     * retry release under the existing recovery claim before replacement can
     * start; callbacks are never redispatched here.
     */
    data class TerminalReceiptCleanupFailed(
        val lifecycle: SessionLifecycleRef,
        val claim: WorkerRecoveryClaim,
        val cleanup: TerminalReceiptCleanupEvidence,
    ) : WorkerRecoveryOutcome
}

internal class WorkerRecoveryException(
    val outcome: WorkerRecoveryOutcome,
    cause: Throwable? = WorkerExitExceptionEvidence.lookup(outcome),
) : IllegalStateException("worker recovery failed: $outcome", cause)

internal sealed interface WorkerRecoveryClaimDecision {
    data class Granted(
        val claim: WorkerRecoveryClaim,
        val workers: OwnerWorkers,
    ) : WorkerRecoveryClaimDecision
    data object NotFenced : WorkerRecoveryClaimDecision
    data class OwnershipMismatch(val lifecycle: SessionLifecycleRef) : WorkerRecoveryClaimDecision
    data class RecoveryInProgress(val claim: WorkerRecoveryClaim) : WorkerRecoveryClaimDecision
    data class AwaitingExit(
        val lifecycle: SessionLifecycleRef,
        val ownership: WorkerOwnership,
    ) : WorkerRecoveryClaimDecision
}

internal class OwnerWorkers(
    val lifecycle: SessionLifecycleRef,
) {
    val terminalOwnership = WorkerOwnership(lifecycle, WorkerKind.DELIVERY_TERMINAL, WorkerGeneration.random())
    val drainOwnership = WorkerOwnership(lifecycle, WorkerKind.OUTBOUND_DRAIN, WorkerGeneration.random())

    lateinit var terminal: DeliveryTerminalWorker.Run
        internal set
    lateinit var drain: OutboundDrainWorker.Run
        internal set

    private var terminalReady = false
    private var drainReady = false
    private var installed = false
    private var terminalExit: WorkerExit? = null
    private var drainExit: WorkerExit? = null

    fun install(terminal: DeliveryTerminalWorker.Run, drain: OutboundDrainWorker.Run) {
        check(terminal.ownership == terminalOwnership)
        check(drain.ownership == drainOwnership)
        this.terminal = terminal
        this.drain = drain
        installed = true
    }

    fun isInstalled(): Boolean = installed

    fun markReady(ownership: WorkerOwnership): Boolean = when (ownership) {
        terminalOwnership -> !terminalReady.also { terminalReady = true }
        drainOwnership -> !drainReady.also { drainReady = true }
        else -> false
    }

    fun bothReady(): Boolean = terminalReady && drainReady

    fun recordExactExit(exit: WorkerExit): Boolean = when (exit.ownership()) {
        terminalOwnership -> if (terminalExit == null) { terminalExit = exit; true } else false
        drainOwnership -> if (drainExit == null) { drainExit = exit; true } else false
        else -> false
    }

    fun exitFor(ownership: WorkerOwnership): WorkerExit? = when (ownership) {
        terminalOwnership -> terminalExit
        drainOwnership -> drainExit
        else -> null
    }

    fun owns(ownership: WorkerOwnership): Boolean =
        ownership == terminalOwnership || ownership == drainOwnership

    fun siblingOf(ownership: WorkerOwnership): WorkerOwnership? = when (ownership) {
        terminalOwnership -> drainOwnership
        drainOwnership -> terminalOwnership
        else -> null
    }
}

internal fun WorkerExit.ownership(): WorkerOwnership = WorkerOwnership(lifecycle, kind, generation)

@JvmInline
internal value class ConnectionAttemptHandle private constructor(
    val value: UUID,
) {
    companion object {
        fun random(): ConnectionAttemptHandle = ConnectionAttemptHandle(UUID.randomUUID())
    }
}

internal sealed interface OutboundLifecycleState {
    data object Stopped : OutboundLifecycleState

    data class Bootstrapping(
        val lifecycle: SessionLifecycleRef,
    ) : OutboundLifecycleState

    data class Open(
        val lifecycle: SessionLifecycleRef,
    ) : OutboundLifecycleState

    data class Active(
        val lifecycle: SessionLifecycleRef,
        val handle: ConnectionAttemptHandle,
        val attempt: DeliveryAttemptRef,
    ) : OutboundLifecycleState

    data class Handoff(
        val lifecycle: SessionLifecycleRef,
        val handle: ConnectionAttemptHandle,
        val previousAttempt: DeliveryAttemptRef?,
        val nextAttempt: DeliveryAttemptRef?,
    ) : OutboundLifecycleState

    data class Closing(
        val lifecycle: SessionLifecycleRef,
        val handle: ConnectionAttemptHandle?,
        val attempt: DeliveryAttemptRef?,
    ) : OutboundLifecycleState

    data class Fenced(
        val lifecycle: SessionLifecycleRef,
        val cause: LifecycleFenceCause,
    ) : OutboundLifecycleState
}

internal sealed interface AttemptCloseOutcome {
    data object Closed : AttemptCloseOutcome
    data object AlreadyClosed : AttemptCloseOutcome
    data object OwnedBySessionShutdown : AttemptCloseOutcome
    data class FencedWithPending(
        val component: LifecyclePendingComponent,
        val pending: Int,
    ) : AttemptCloseOutcome
    data object Stale : AttemptCloseOutcome
}

internal data class AttemptActivation(
    val lifecycle: SessionLifecycleRef,
    val handle: ConnectionAttemptHandle,
    val bootstrap: OutboundQueue.AttemptBootstrap,
    val bridge: XmppEventBridge,
)

internal sealed interface ResumeHandoffOutcome {
    data object Committed : ResumeHandoffOutcome
    data object Rejected : ResumeHandoffOutcome
}

internal enum class LifecyclePendingComponent {
    ACTIVATION_COMPENSATION,
    ATTEMPT_FINALIZATION,
    ATTEMPT_LEASES,
    NATIVE_PRODUCER,
    NATIVE_DISCONNECT,
    NATIVE_CLIENT_CLOSE,
    OUTBOUND_DRAIN,
    TERMINAL_DRAIN,
}

internal enum class DurableCleanupOperation {
    JOURNAL_INSPECTION,
    JOURNAL_FENCE,
    RESUME_RETIREMENT,
    ACTIVE_SESSION_CLEANUP,
}

internal enum class DurableCleanupFailureCause {
    IO_FAILURE,
}

internal sealed interface LifecycleShutdownOutcome {
    data object Stopped : LifecycleShutdownOutcome

    data class WorkerFenced(
        val lifecycle: SessionLifecycleRef,
        val cause: LifecycleFenceCause,
    ) : LifecycleShutdownOutcome

    data class FencedWithPending(
        val lifecycle: SessionLifecycleRef,
        val component: LifecyclePendingComponent,
        val pending: Int,
    ) : LifecycleShutdownOutcome

    data object AttemptClosed : LifecycleShutdownOutcome
    data object Stale : LifecycleShutdownOutcome
}

internal sealed interface LifecycleShutdownTarget {
    data class CurrentOwner(
        val lifecycle: SessionLifecycleRef,
    ) : LifecycleShutdownTarget

    data class ExactAttempt(
        val lifecycle: SessionLifecycleRef,
        val attempt: DeliveryAttemptRef,
    ) : LifecycleShutdownTarget
}

internal sealed interface LiveOutboundPurpose {
    data class MessageSend(
        val source: social.waddle.android.client.prefs.DeliverySource,
    ) : LiveOutboundPurpose

    data object Drain : LiveOutboundPurpose
}

internal sealed interface OutboundAdmissionLease {
    val lifecycle: SessionLifecycleRef
    val capability: LifecycleOperationRegistry.Lease

    class OfflineOutbound private constructor(
        val source: social.waddle.android.client.prefs.DeliverySource,
        override val capability: LifecycleOperationRegistry.Lease,
    ) : OutboundAdmissionLease {
        override val lifecycle: SessionLifecycleRef get() = capability.lifecycle
        val attempt: DeliveryAttemptRef? get() = capability.attempt

        companion object {
            internal fun issue(
                source: social.waddle.android.client.prefs.DeliverySource,
                capability: LifecycleOperationRegistry.Lease,
            ): OfflineOutbound = OfflineOutbound(source, capability)
        }
    }

    class LiveOutbound private constructor(
        val client: social.waddle.client.ffi.WaddleClientInterface,
        val purpose: LiveOutboundPurpose,
        override val capability: LifecycleOperationRegistry.Lease,
    ) : OutboundAdmissionLease {
        override val lifecycle: SessionLifecycleRef get() = capability.lifecycle
        val attempt: DeliveryAttemptRef get() = requireNotNull(capability.attempt)

        companion object {
            internal fun issue(
                client: social.waddle.client.ffi.WaddleClientInterface,
                purpose: LiveOutboundPurpose,
                capability: LifecycleOperationRegistry.Lease,
            ): LiveOutbound {
                requireNotNull(capability.attempt) { "live admission capability requires an attempt" }
                return LiveOutbound(client, purpose, capability)
            }
        }
    }

    class Terminal private constructor(
        override val capability: LifecycleOperationRegistry.Lease,
    ) : OutboundAdmissionLease {
        override val lifecycle: SessionLifecycleRef get() = capability.lifecycle
        val attempt: DeliveryAttemptRef get() = requireNotNull(capability.attempt)

        companion object {
            internal fun issue(
                capability: LifecycleOperationRegistry.Lease,
            ): Terminal {
                requireNotNull(capability.attempt) { "terminal admission capability requires an attempt" }
                return Terminal(capability)
            }
        }
    }
}

internal sealed interface OutboundAdmissionResult {
    data class Granted(
        val lease: OutboundAdmissionLease,
    ) : OutboundAdmissionResult

    data object OwnerMismatch : OutboundAdmissionResult
    data object LifecycleUnavailable : OutboundAdmissionResult
}

internal class LifecycleTransitionException(
    val lifecycle: SessionLifecycleRef,
    val component: LifecyclePendingComponent,
    val pending: Int,
    cause: Throwable? = null,
) : IllegalStateException(
    "outbound lifecycle fenced with pending $component work ($pending)",
    cause,
)

internal enum class OutboundLifecyclePhase {
    TERMINAL_WORKER_READY,
    DRAIN_WORKER_READY,
    STARTUP_READINESS_LOST,
    SHUTDOWN_OWNER_FINALIZED,
    AWAITING_REQUESTED_WORKER_EXIT_INSTALLED,
    ATTEMPT_JOURNALING,
    ATTEMPT_JOURNALED,
    RESUME_REGISTERED,
    DRAIN_BOUND,
    ACTIVE_SESSION_PUBLISHED,
    ATTEMPT_PUBLISHED,
    ROTATION_JOURNALED,
    ROTATION_RESUME_REGISTERED,
    ROTATION_DRAIN_BOUND,
    ROTATION_ACTIVE_SESSION_PUBLISHED,
    ROTATION_PUBLISHED,
}

internal fun interface OutboundLifecyclePhaseObserver {
    suspend fun after(phase: OutboundLifecyclePhase)

    companion object {
        val NONE = OutboundLifecyclePhaseObserver { }
    }
}
