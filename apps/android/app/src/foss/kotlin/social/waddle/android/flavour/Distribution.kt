package social.waddle.android.flavour

import android.content.Context
import social.waddle.android.push.FcmTokens

/** The FOSS build: UnifiedPush or a background connection; no Google code. */
object Distribution : DistributionHooks {
    override val stayConnectedSupported: Boolean = true

    override fun initialize(context: Context) = Unit

    override fun fcmTokens(): FcmTokens? = null
}
