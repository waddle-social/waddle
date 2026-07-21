package social.waddle.android.client

import social.waddle.android.client.prefs.DeliveryAttemptRef

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
    val bootstrap: DeliveryJournalStore.AttemptBootstrap,
    val bridge: XmppEventBridge,
)
internal sealed interface ResumeHandoffOutcome { data object Committed : ResumeHandoffOutcome
data object Rejected : ResumeHandoffOutcome }
internal enum class LifecyclePendingComponent { ACTIVATION_COMPENSATION, ATTEMPT_FINALIZATION, ATTEMPT_LEASES, NATIVE_PRODUCER, NATIVE_DISCONNECT, NATIVE_CLIENT_CLOSE, OUTBOUND_DRAIN, TERMINAL_DRAIN }
internal enum class DurableCleanupOperation { JOURNAL_INSPECTION, JOURNAL_FENCE, RESUME_RETIREMENT, ACTIVE_SESSION_CLEANUP }
internal enum class DurableCleanupFailureCause { IO_FAILURE }
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
internal sealed interface LifecycleShutdownTarget { data class CurrentOwner(val lifecycle: SessionLifecycleRef) : LifecycleShutdownTarget
data class ExactAttempt(val lifecycle: SessionLifecycleRef, val attempt: DeliveryAttemptRef) : LifecycleShutdownTarget }
internal class LifecycleTransitionException(
    val lifecycle: SessionLifecycleRef,
    val component: LifecyclePendingComponent,
    val pending: Int,
    cause: Throwable? = null,
) : IllegalStateException(
    "outbound lifecycle fenced with pending $component work ($pending)",
    cause,
)
internal enum class OutboundLifecyclePhase { TERMINAL_WORKER_READY, DRAIN_WORKER_READY, STARTUP_READINESS_LOST, SHUTDOWN_OWNER_FINALIZED, AWAITING_REQUESTED_WORKER_EXIT_INSTALLED, ATTEMPT_JOURNALING, ATTEMPT_JOURNALED, RESUME_REGISTERED, DRAIN_BOUND, ACTIVE_SESSION_PUBLISHED, ATTEMPT_PUBLISHED, ROTATION_JOURNALED, ROTATION_RESUME_REGISTERED, ROTATION_DRAIN_BOUND, ROTATION_ACTIVE_SESSION_PUBLISHED, ROTATION_PUBLISHED }
internal fun interface OutboundLifecyclePhaseObserver { suspend fun after(phase: OutboundLifecyclePhase)
companion object { val NONE = OutboundLifecyclePhaseObserver { } } }
