package social.waddle.android.push

import kotlinx.coroutines.launch
import org.unifiedpush.android.connector.FailedReason
import org.unifiedpush.android.connector.PushService
import org.unifiedpush.android.connector.data.PushEndpoint
import org.unifiedpush.android.connector.data.PushMessage
import social.waddle.android.WaddleApplication

/**
 * UnifiedPush callbacks. The connector binds this service only briefly,
 * so every callback hands off to the application scope and returns;
 * handling reads only local state and never waits for a session.
 */
class WaddlePushService : PushService() {
    private val graph get() = (application as WaddleApplication).graph

    override fun onNewEndpoint(endpoint: PushEndpoint, instance: String) {
        // The server speaks Web Push only: an endpoint without the
        // connector's key pair cannot receive encrypted payloads.
        val keys = endpoint.pubKeySet ?: return
        graph.applicationScope.launch {
            val runtime = graph.runtimeForPushInstance(instance) ?: return@launch
            runCatching { runtime.push.onUnifiedPushEndpoint(instance, endpoint.url, keys.pubKey, keys.auth) }
        }
    }

    override fun onMessage(message: PushMessage, instance: String) {
        graph.applicationScope.launch {
            // The instance names the account (it is scoped to origin and JID).
            val runtime = graph.runtimeForPushInstance(instance) ?: return@launch
            runCatching { runtime.pushMessageHandler.handle(instance, message.content, message.decrypted) }
        }
    }

    override fun onRegistrationFailed(reason: FailedReason, instance: String) {
        // Nothing to undo: without an endpoint the account stays on the
        // stay-connected path, and the next foreground start retries.
        graph.pushDistributor.refresh()
    }

    override fun onUnregistered(instance: String) {
        graph.applicationScope.launch {
            val runtime = graph.runtimeForPushInstance(instance) ?: return@launch
            runCatching { runtime.push.onUnifiedPushUnregistered(instance) }
        }
    }
}
