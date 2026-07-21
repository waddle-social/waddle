package social.waddle.android.client

import social.waddle.android.client.prefs.DeliveryAttemptRef
import java.util.UUID

internal enum class WorkerKind { OUTBOUND_DRAIN, DELIVERY_TERMINAL }

@JvmInline internal value class WorkerGeneration private constructor(val value: UUID) { companion object { fun random() = WorkerGeneration(UUID.randomUUID()) } }
internal data class WorkerOwnership(val lifecycle: SessionLifecycleRef, val kind: WorkerKind, val generation: WorkerGeneration)
internal sealed interface WorkerFailureKind { data object DEPENDENCY_FAILURE : WorkerFailureKind
data class TERMINAL_RECEIPT_APPLICATION(val failure: TerminalReceiptApplicationFailure) : WorkerFailureKind }
internal sealed interface WorkerExitReason { data object RequestedStop : WorkerExitReason
data object OwnerScopeCancelled : WorkerExitReason
data object UnexpectedReturn : WorkerExitReason
data class UnexpectedFailure(val kind: WorkerFailureKind) : WorkerExitReason }
internal data class WorkerExit(val lifecycle: SessionLifecycleRef, val generation: WorkerGeneration, val kind: WorkerKind, val reason: WorkerExitReason)
internal data class BootstrapWorkerExitFailure(val exit: WorkerExit)
internal data class WorkerFence(val exit: WorkerExit)
internal sealed interface LifecycleFenceCause { data class WorkerExited(val fence: WorkerFence) : LifecycleFenceCause
data class AwaitingRequestedWorkerExit(val ownership: WorkerOwnership) : LifecycleFenceCause }
internal sealed interface WorkerExitGateDecision { data object Ignore : WorkerExitGateDecision
data object RecordOnly : WorkerExitGateDecision
data class Fence(val cause: LifecycleFenceCause.WorkerExited) : WorkerExitGateDecision }

internal fun decideWorkerExitGate(state: OutboundLifecycleState, exactOwner: Boolean, firstExactExit: Boolean, exit: WorkerExit): WorkerExitGateDecision {
    if (!exactOwner || !firstExactExit) return WorkerExitGateDecision.Ignore
    val exited = LifecycleFenceCause.WorkerExited(WorkerFence(exit))
    val fenced = state as? OutboundLifecycleState.Fenced
    if (fenced != null) {
        val cause = fenced.cause
        if (cause is LifecycleFenceCause.WorkerExited) return WorkerExitGateDecision.Ignore
        val awaiting = cause as LifecycleFenceCause.AwaitingRequestedWorkerExit
        return if (exit.ownership() == awaiting.ownership || exit.reason !is WorkerExitReason.RequestedStop) WorkerExitGateDecision.Fence(exited) else WorkerExitGateDecision.RecordOnly
    }
    return if (state is OutboundLifecycleState.Closing && exit.reason is WorkerExitReason.RequestedStop) WorkerExitGateDecision.RecordOnly else WorkerExitGateDecision.Fence(exited)
}

internal sealed interface WorkerAwaitOutcome { data class Exited(val exit: WorkerExit) : WorkerAwaitOutcome
data object TimedOut : WorkerAwaitOutcome }
internal data class TerminalWorkerFailure(val kind: WorkerFailureKind)
internal sealed interface TerminalCommandOutcome { data object Committed : TerminalCommandOutcome
data object WorkerUnavailable : TerminalCommandOutcome
data class Failed(val failure: TerminalWorkerFailure) : TerminalCommandOutcome }
internal sealed interface DrainSignalOutcome { data object Accepted : DrainSignalOutcome
data object Mismatch : DrainSignalOutcome
data object WorkerUnavailable : DrainSignalOutcome }
internal class TerminalWorkerUnavailableException : IllegalStateException()
internal class TerminalWorkerCommandFailedException(val failure: TerminalWorkerFailure) : IllegalStateException()
internal fun requireTerminalCommitted(outcome: TerminalCommandOutcome) {
    when (outcome) { TerminalCommandOutcome.Committed -> Unit
    TerminalCommandOutcome.WorkerUnavailable -> throw TerminalWorkerUnavailableException()
    is TerminalCommandOutcome.Failed -> throw TerminalWorkerCommandFailedException(outcome.failure) }
}

@JvmInline internal value class WorkerRecoveryToken private constructor(val value: UUID) { companion object { fun random() = WorkerRecoveryToken(UUID.randomUUID()) } }
internal data class WorkerRecoveryClaim(val lifecycle: SessionLifecycleRef, val fence: WorkerFence, val token: WorkerRecoveryToken)
internal sealed interface WorkerRecoveryOutcome {
    data object Recovered : WorkerRecoveryOutcome
    data object NotFenced : WorkerRecoveryOutcome
    data class OwnershipMismatch(val requested: SessionLifecycleRef, val actual: SessionLifecycleRef?) : WorkerRecoveryOutcome
    data class WorkerFenced(val lifecycle: SessionLifecycleRef, val cause: LifecycleFenceCause) : WorkerRecoveryOutcome
    data class RecoveryInProgress(val claim: WorkerRecoveryClaim) : WorkerRecoveryOutcome
    data class RetainedOperationsPending(val lifecycle: SessionLifecycleRef, val claim: WorkerRecoveryClaim, val count: Int) : WorkerRecoveryOutcome
    data class WorkerExitPending(val lifecycle: SessionLifecycleRef, val ownership: WorkerOwnership) : WorkerRecoveryOutcome
    data class DurableCleanupPending(val lifecycle: SessionLifecycleRef, val claim: WorkerRecoveryClaim, val component: LifecyclePendingComponent, val count: Int, val operation: DurableCleanupOperation, val attempt: DeliveryAttemptRef?) : WorkerRecoveryOutcome
    data class DurableCleanupFailed(val lifecycle: SessionLifecycleRef, val claim: WorkerRecoveryClaim, val component: LifecyclePendingComponent, val count: Int, val operation: DurableCleanupOperation, val cause: DurableCleanupFailureCause, val attempt: DeliveryAttemptRef?) : WorkerRecoveryOutcome
    data class TerminalReceiptCleanupFailed(val lifecycle: SessionLifecycleRef, val claim: WorkerRecoveryClaim, val cleanup: TerminalReceiptCleanupEvidence) : WorkerRecoveryOutcome
}
internal class WorkerRecoveryException(val outcome: WorkerRecoveryOutcome, cause: Throwable?) : IllegalStateException("worker recovery failed: $outcome", cause)
internal sealed interface WorkerRecoveryClaimDecision { data class Granted(val claim: WorkerRecoveryClaim, val workers: OwnerWorkers) : WorkerRecoveryClaimDecision
data object NotFenced : WorkerRecoveryClaimDecision
data class OwnershipMismatch(val lifecycle: SessionLifecycleRef) : WorkerRecoveryClaimDecision
data class RecoveryInProgress(val claim: WorkerRecoveryClaim) : WorkerRecoveryClaimDecision
data class AwaitingExit(val lifecycle: SessionLifecycleRef, val ownership: WorkerOwnership) : WorkerRecoveryClaimDecision }

internal class OwnerWorkers(val lifecycle: SessionLifecycleRef) {
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
    fun isInstalled() = installed
    fun terminalOrNull(): DeliveryTerminalWorker.Run? = if (::terminal.isInitialized) terminal else null
    fun drainOrNull(): OutboundDrainWorker.Run? = if (::drain.isInitialized) drain else null
    fun markReady(ownership: WorkerOwnership): Boolean = when (ownership) { terminalOwnership -> !terminalReady.also {
        terminalReady = true
    }
    drainOwnership -> !drainReady.also { drainReady = true }
    else -> false }
    fun bothReady() = terminalReady && drainReady
    fun recordExactExit(exit: WorkerExit): Boolean = when (exit.ownership()) { terminalOwnership -> if (terminalExit == null) {
        terminalExit = exit
        true
    } else {
        false
    }
    drainOwnership -> if (drainExit == null) {
        drainExit = exit
    true
    } else {
        false
    }
    else -> false }
    fun exitFor(ownership: WorkerOwnership) = when (ownership) { terminalOwnership -> terminalExit
    drainOwnership -> drainExit
    else -> null }
    fun owns(ownership: WorkerOwnership) = ownership == terminalOwnership || ownership == drainOwnership
    fun siblingOf(ownership: WorkerOwnership): WorkerOwnership? = when (ownership) { terminalOwnership -> drainOwnership
    drainOwnership -> terminalOwnership
    else -> null }
}
internal fun WorkerExit.ownership() = WorkerOwnership(lifecycle, kind, generation)
