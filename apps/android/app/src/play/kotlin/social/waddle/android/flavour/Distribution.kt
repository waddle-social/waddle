package social.waddle.android.flavour

import android.content.Context
import com.google.firebase.FirebaseApp
import com.google.firebase.FirebaseOptions
import social.waddle.android.BuildConfig
import social.waddle.android.push.FcmTokens
import social.waddle.android.push.FirebaseFcmTokens

/**
 * The Play build: FCM by default (UnifiedPush when the user picks a
 * distributor), and no background connection service.
 *
 * FCM is configured from build values, never `google-services.json`, and
 * the messaging library is the only Google product present: no analytics,
 * crash or other reporting. A build without the values has no FCM.
 */
object Distribution : DistributionHooks {
    override val stayConnectedSupported: Boolean = false

    @Volatile
    private var firebase: FirebaseApp? = null

    override fun initialize(context: Context) {
        val options = firebaseOptions() ?: return
        firebase = FirebaseApp.getApps(context).firstOrNull() ?: FirebaseApp.initializeApp(context, options)
    }

    override fun fcmTokens(): FcmTokens? = firebase?.let { FirebaseFcmTokens() }

    private fun firebaseOptions(): FirebaseOptions? {
        val values = listOf(
            BuildConfig.WADDLE_FIREBASE_PROJECT_ID,
            BuildConfig.WADDLE_FIREBASE_APP_ID,
            BuildConfig.WADDLE_FIREBASE_API_KEY,
            BuildConfig.WADDLE_FIREBASE_SENDER_ID,
        )
        if (values.any { it.isBlank() }) return null
        return FirebaseOptions.Builder()
            .setProjectId(BuildConfig.WADDLE_FIREBASE_PROJECT_ID)
            .setApplicationId(BuildConfig.WADDLE_FIREBASE_APP_ID)
            .setApiKey(BuildConfig.WADDLE_FIREBASE_API_KEY)
            .setGcmSenderId(BuildConfig.WADDLE_FIREBASE_SENDER_ID)
            .build()
    }
}
