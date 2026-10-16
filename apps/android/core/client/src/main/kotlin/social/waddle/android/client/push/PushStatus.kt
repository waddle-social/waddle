package social.waddle.android.client.push

import social.waddle.android.client.prefs.PushState

/** Where push delivery stands for an account, derived from [PushState]. */
enum class PushStatus {
    /** No endpoint: no distributor registered for this account yet. */
    NONE,

    /** The server runs no Web Push provider. */
    UNSUPPORTED,

    /** An endpoint exists and the Push Service registration is underway. */
    PENDING,

    /** The server pushes to this device. */
    ACTIVE,

    /** The Push Service refused the registration. */
    FAILED,
}

/**
 * [state]'s status for [owner]. Pure and computed from persisted state,
 * so a cold start knows the delivery mode before any stream is up.
 */
fun pushStatusOf(state: PushState?, owner: String?): PushStatus {
    val mine = state?.takeIf { owner != null && it.owner == owner } ?: return PushStatus.NONE
    val endpoint = mine.endpoint
    val device = mine.device
    return when {
        mine.unsupported -> PushStatus.UNSUPPORTED
        endpoint == null -> PushStatus.NONE
        device != null && device.enabled && device.endpoint == endpoint -> PushStatus.ACTIVE
        mine.failed -> PushStatus.FAILED
        else -> PushStatus.PENDING
    }
}
