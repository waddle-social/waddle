package social.waddle.android.client.push

import social.waddle.android.client.prefs.PushEndpoint
import social.waddle.android.client.prefs.PushState

/** Where push delivery stands for an account, derived from [PushState]. */
enum class PushStatus {
    /** No endpoint: no transport registered for this account yet. */
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
    return when (val endpoint = mine.endpoint) {
        // FCM needs neither a Web Push provider at the server nor its key.
        is PushEndpoint.Fcm -> deviceStatusOf(mine, endpoint)
        is PushEndpoint.WebPush -> when {
            mine.unsupported -> PushStatus.UNSUPPORTED
            // The server rotated its key: pushes it signs no longer reach the
            // old endpoint until the distributor hands out a new one.
            mine.advertised != null && endpoint.vapidKid != mine.advertised.kid -> PushStatus.PENDING
            else -> deviceStatusOf(mine, endpoint)
        }
        null -> if (mine.unsupported) PushStatus.UNSUPPORTED else PushStatus.NONE
    }
}

/** Whether the device registered for [endpoint] is enabled, else why not. */
private fun deviceStatusOf(state: PushState, endpoint: PushEndpoint): PushStatus {
    val device = state.device
    return when {
        device != null && device.enabled && device.endpoint == endpoint -> PushStatus.ACTIVE
        state.failed -> PushStatus.FAILED
        else -> PushStatus.PENDING
    }
}
