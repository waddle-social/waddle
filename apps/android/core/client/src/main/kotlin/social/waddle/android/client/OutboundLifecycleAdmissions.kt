package social.waddle.android.client

import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliverySource
import social.waddle.android.client.session.ActiveSession
import java.util.UUID

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
        is OutboundLifecycleState.Closing,
        is OutboundLifecycleState.Handoff,
        OutboundLifecycleState.Stopped,
        -> return OutboundReservationClaim.LifecycleUnavailable
    }
    return OutboundReservationClaim.Granted(
        AdmissionReservation(lifecycle, attempt, UUID.randomUUID()),
    )
}

internal fun createAdmissionReservation(
    state: OutboundLifecycleState,
    expectedOwnerBareJid: String?,
    expectedAttempt: DeliveryAttemptRef?,
    requireActive: Boolean,
): AdmissionReservation? {
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
        is OutboundLifecycleState.Closing,
        is OutboundLifecycleState.Handoff,
        OutboundLifecycleState.Stopped,
        -> return null
    }
    if (expectedAttempt != null && attempt != expectedAttempt) return null
    return AdmissionReservation(lifecycle, attempt, UUID.randomUUID())
}

internal suspend fun materializeOutboundAdmission(
    activeSession: ActiveSession,
    reservation: AdmissionReservation,
    source: DeliverySource,
): OutboundAdmissionResult.Granted {
    val attempt = reservation.attempt
    if (attempt == null) {
        return OutboundAdmissionResult.Granted(
            OutboundAdmissionLease.OfflineOutbound(
                reservation.lifecycle,
                source,
                attempt,
                reservation.token,
            ),
        )
    }
    val client = activeSession.clientAtAttempt(attempt)
    val lease = if (client == null) {
        OutboundAdmissionLease.OfflineOutbound(
            reservation.lifecycle,
            source,
            attempt,
            reservation.token,
        )
    } else {
        OutboundAdmissionLease.LiveOutbound(
            reservation.lifecycle,
            attempt,
            client,
            LiveOutboundPurpose.MessageSend(source),
            reservation.token,
        )
    }
    return OutboundAdmissionResult.Granted(lease)
}
