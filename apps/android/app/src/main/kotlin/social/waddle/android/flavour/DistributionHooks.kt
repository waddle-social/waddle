package social.waddle.android.flavour

import android.content.Context
import social.waddle.android.push.FcmTokens

/**
 * What differs between the FOSS build (Obtainium, F-Droid) and the Play
 * build. Each flavour's source set provides `Distribution`, an object
 * implementing this.
 */
interface DistributionHooks {
    /**
     * Whether the build may hold the stream in the background with a
     * foreground service. The Play build relies on push instead.
     */
    val stayConnectedSupported: Boolean

    /** Process start, before the app graph exists. */
    fun initialize(context: Context)

    /** FCM, when the build has it and it is configured; `null` otherwise. */
    fun fcmTokens(): FcmTokens?
}
