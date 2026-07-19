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
internal enum class WorkerFailureKind {
    DEPENDENCY_FAILURE,
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
    data class OwnershipMismatch(val lifecycle: SessionLifecycleRef) : WorkerRecoveryOutcome
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
        val component: LifecyclePendingComponent,
        val pending: Int,
    ) : WorkerRecoveryOutcome
}

internal class WorkerRecoveryException(
    val outcome: WorkerRecoveryOutcome,
) : IllegalStateException()

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
    private var terminalExit: WorkerExit? = null
    private var drainExit: WorkerExit? = null

    fun install(terminal: DeliveryTerminalWorker.Run, drain: OutboundDrainWorker.Run) {
        check(terminal.ownership == terminalOwnership)
        check(drain.ownership == drainOwnership)
        this.terminal = terminal
        this.drain = drain
    }

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
    val token: UUID

    data class OfflineOutbound(
        override val lifecycle: SessionLifecycleRef,
        val source: social.waddle.android.client.prefs.DeliverySource,
        val attempt: DeliveryAttemptRef?,
        override val token: UUID,
    ) : OutboundAdmissionLease

    data class LiveOutbound(
        override val lifecycle: SessionLifecycleRef,
        val attempt: DeliveryAttemptRef,
        val client: social.waddle.client.ffi.WaddleClientInterface,
        val purpose: LiveOutboundPurpose,
        override val token: UUID,
    ) : OutboundAdmissionLease

    data class Terminal(
        override val lifecycle: SessionLifecycleRef,
        val attempt: DeliveryAttemptRef,
        override val token: UUID,
    ) : OutboundAdmissionLease
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
