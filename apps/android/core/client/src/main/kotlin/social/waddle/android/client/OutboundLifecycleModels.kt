package social.waddle.android.client

import kotlinx.coroutines.CompletableDeferred
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.client.ffi.WaddleClientInterface
import java.util.UUID

internal fun OutboundLifecycleState.lifecycleOrNull(): SessionLifecycleRef? =
    when (this) {
        is OutboundLifecycleState.Open -> lifecycle
        is OutboundLifecycleState.Active -> lifecycle
        is OutboundLifecycleState.Handoff -> lifecycle
        is OutboundLifecycleState.Closing -> lifecycle
        OutboundLifecycleState.Stopped -> null
    }

internal fun OutboundLifecycleState.attemptOrNull(): DeliveryAttemptRef? =
    when (this) {
        is OutboundLifecycleState.Active -> attempt
        is OutboundLifecycleState.Handoff -> nextAttempt ?: previousAttempt
        is OutboundLifecycleState.Closing -> attempt
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

internal data class OperationCapability(
    val lifecycle: SessionLifecycleRef,
    val attempt: DeliveryAttemptRef?,
    val operationId: UUID,
)

/**
 * A coordinator creates one registry for one lifecycle only. Consequently a
 * late completion from a retired lifecycle cannot remove work retained by a
 * replacement owner, even when both owners have the same bare JID.
 *
 * All methods are called under OutboundLifecycleCoordinator's short gate.
 */
internal class LifecycleOperationRegistry(
    private val lifecycle: SessionLifecycleRef,
) {
    private val retained = mutableMapOf<UUID, OperationCapability>()
    private var emptyWaiter: CompletableDeferred<Unit>? = null

    fun retain(capability: OperationCapability): Boolean {
        if (capability.lifecycle != lifecycle) return false
        retained[capability.operationId] = capability
        return true
    }

    fun release(capability: OperationCapability) {
        if (retained[capability.operationId] != capability) return
        retained.remove(capability.operationId)
        if (retained.isEmpty()) {
            emptyWaiter?.complete(Unit)
            emptyWaiter = null
        }
    }

    fun owns(capability: OperationCapability): Boolean =
        retained[capability.operationId] == capability

    fun waiterIfRetained(): CompletableDeferred<Unit>? =
        if (retained.isEmpty()) null else emptyWaiter ?: CompletableDeferred<Unit>().also {
            emptyWaiter = it
        }

    fun retainedCount(): Int = retained.size
}

internal data class TransportConstructionClaim(
    val lifecycle: SessionLifecycleRef,
    val handle: ConnectionAttemptHandle,
    val attempt: DeliveryAttemptRef,
    val capability: OperationCapability,
)

internal sealed interface TransportAttachOutcome {
    data object Attached : TransportAttachOutcome

    /** The caller still owns and must close the just-constructed client. */
    data object SupersededAndClose : TransportAttachOutcome
}

internal data class AdmissionReservation(
    val lifecycle: SessionLifecycleRef,
    val attempt: DeliveryAttemptRef?,
    val token: UUID,
)

internal sealed interface OutboundReservationClaim {
    data class Granted(
        val reservation: AdmissionReservation,
    ) : OutboundReservationClaim

    data object OwnerMismatch : OutboundReservationClaim
    data object LifecycleUnavailable : OutboundReservationClaim
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

internal fun isTerminalRecoveryEligible(
    state: OutboundLifecycleState,
    pending: LifecycleShutdownOutcome.FencedWithPending?,
    lifecycle: SessionLifecycleRef,
    leasesEmpty: Boolean,
): Boolean {
    if (state !is OutboundLifecycleState.Closing) return false
    if (state.lifecycle != lifecycle) return false
    if (pending?.lifecycle != lifecycle) return false
    if (pending.component != LifecyclePendingComponent.TERMINAL_DRAIN) return false
    return leasesEmpty
}

internal fun pendingLifecycleShutdown(
    lifecycle: SessionLifecycleRef,
    component: LifecyclePendingComponent,
    pending: Int,
) = LifecycleShutdownOutcome.FencedWithPending(
    lifecycle = lifecycle,
    component = component,
    pending = pending.coerceAtLeast(1),
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
