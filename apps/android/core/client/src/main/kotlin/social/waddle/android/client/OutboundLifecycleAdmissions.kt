package social.waddle.android.client

import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliverySource
import social.waddle.android.client.session.ActiveSession

/**
 * The messenger owns its finally boundary while the coordinator owns the
 * registry. Keeping this port typed makes that ownership handoff explicit.
 */
internal fun interface OutboundAdmissionReleaseOperations {
    suspend fun release(
        lifecycle: OutboundLifecycleCoordinator,
        lease: OutboundAdmissionLease,
    ): LifecycleReleaseOutcome

    companion object {
        val COORDINATOR = OutboundAdmissionReleaseOperations { lifecycle, lease ->
            lifecycle.releaseAdmission(lease)
        }
    }
}

internal fun classifyOutboundReservation(
    state: OutboundLifecycleState,
    expectedOwnerBareJid: String?,
): OutboundReservationClaim {
    val lifecycle = state.lifecycleOrNull()
        ?: return OutboundReservationClaim.LifecycleUnavailable
    if (
        expectedOwnerBareJid != null &&
        lifecycle.ownerBareJid != expectedOwnerBareJid
    ) {
        return OutboundReservationClaim.OwnerMismatch
    }
    val attempt = when (state) {
        is OutboundLifecycleState.Active -> state.attempt
        is OutboundLifecycleState.Open -> null
        is OutboundLifecycleState.Bootstrapping,
        is OutboundLifecycleState.Fenced,
        is OutboundLifecycleState.Closing,
        is OutboundLifecycleState.Handoff,
        OutboundLifecycleState.Stopped,
        -> return OutboundReservationClaim.LifecycleUnavailable
    }
    return OutboundReservationClaim.Granted(
        AdmissionCandidate(lifecycle, attempt),
    )
}

internal fun createAdmissionCandidate(
    state: OutboundLifecycleState,
    expectedOwnerBareJid: String?,
    expectedAttempt: DeliveryAttemptRef?,
    requireActive: Boolean,
): AdmissionCandidate? {
    val lifecycle = state.lifecycleOrNull() ?: return null
    if (
        expectedOwnerBareJid != null &&
        lifecycle.ownerBareJid != expectedOwnerBareJid
    ) {
        return null
    }
    val attempt = when (state) {
        is OutboundLifecycleState.Active -> state.attempt
        is OutboundLifecycleState.Open -> if (requireActive) return null else null
        is OutboundLifecycleState.Bootstrapping,
        is OutboundLifecycleState.Fenced,
        is OutboundLifecycleState.Closing,
        is OutboundLifecycleState.Handoff,
        OutboundLifecycleState.Stopped,
        -> return null
    }
    if (expectedAttempt != null && attempt != expectedAttempt) return null
    return AdmissionCandidate(lifecycle, attempt)
}

internal suspend fun materializeOutboundAdmission(
    activeSession: ActiveSession,
    reservation: RetainedAdmission,
    source: DeliverySource,
): OutboundAdmissionResult.Granted {
    val capability = reservation.capability
    val attempt = reservation.attempt
    if (attempt == null) {
        return OutboundAdmissionResult.Granted(
            OutboundAdmissionLease.OfflineOutbound.issue(
                source,
                capability,
            ),
        )
    }
    val client = activeSession.clientAtAttempt(attempt)
    val lease = if (client == null) {
        OutboundAdmissionLease.OfflineOutbound.issue(
            source,
            capability,
        )
    } else {
        OutboundAdmissionLease.LiveOutbound.issue(
            client,
            LiveOutboundPurpose.MessageSend(source),
            capability,
        )
    }
    return OutboundAdmissionResult.Granted(lease)
}
