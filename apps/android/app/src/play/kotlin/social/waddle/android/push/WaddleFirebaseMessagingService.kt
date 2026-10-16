package social.waddle.android.push

import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeoutOrNull
import social.waddle.android.WaddleApplication

/**
 * FCM callbacks. Messages carry only the Push Service's ciphertext; the
 * coordinator decrypts them with this install's keys.
 */
class WaddleFirebaseMessagingService : FirebaseMessagingService() {
    private val graph get() = (application as WaddleApplication).graph

    override fun onNewToken(token: String) {
        graph.applicationScope.launch { runCatching { graph.push.onFcmToken(token) } }
    }

    override fun onMessageReceived(message: RemoteMessage) {
        // Runs on a background thread, and the process may be frozen as
        // soon as this returns: finish the notification here, within the
        // time FCM allows a high-priority message.
        runBlocking {
            withTimeoutOrNull(MESSAGE_BUDGET_MILLIS) {
                runCatching { graph.push.onFcmMessage(message.data) }
            }
        }
    }

    private companion object {
        /** Under FCM's ~20 s; covers the handler's live-stream grace period. */
        const val MESSAGE_BUDGET_MILLIS = 15_000L
    }
}
