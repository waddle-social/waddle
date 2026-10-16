package social.waddle.android.push

import android.app.Activity
import android.content.Context
import android.content.pm.PackageManager
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import org.unifiedpush.android.connector.UnifiedPush
import social.waddle.android.R
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.prefs.PushEndpoint
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.prefs.VapidKey

/**
 * The UnifiedPush transport ([PushCoordinator] decides when it is used):
 * which distributor delivers, asking it
 * for an endpoint bound to the Push Service's VAPID key, and handing the
 * endpoint it returns to the session (which registers it at the server).
 *
 * Choosing a distributor may show UI, which Android only allows from the
 * foreground, so that only happens from [onForeground]; every other path
 * registers only with a distributor already chosen.
 */
class PushDistributor(
    private val context: Context,
    private val scope: CoroutineScope,
    private val sessionManager: XmppSessionManager,
    private val sessionPrefs: SessionPrefs,
    private val userPrefs: UserPrefs,
) {
    private val _distributorLabel = MutableStateFlow<String?>(null)

    /** The chosen distributor's app name; `null` when none is chosen. */
    val distributorLabel: StateFlow<String?> = _distributorLabel.asStateFlow()

    private val _installed = MutableStateFlow(false)

    /** Whether any distributor app is installed at all. */
    val installed: StateFlow<Boolean> = _installed.asStateFlow()

    /** Re-read which distributors exist and which one is chosen. */
    fun refresh() {
        _installed.value = UnifiedPush.getDistributors(context).isNotEmpty()
        _distributorLabel.value = UnifiedPush.getAckDistributor(context)?.let(::labelOf)
    }

    /**
     * The app is in the foreground ([activity] started): settle on a
     * distributor if none is chosen yet, then (re-)register. UnifiedPush
     * asks apps to register on every start; it is idempotent.
     */
    fun onForeground(activity: Activity) {
        refresh()
        scope.launch {
            val key = currentKey() ?: return@launch
            if (UnifiedPush.getAckDistributor(context) != null) {
                register(key)
            } else if (_installed.value) {
                activity.runOnUiThread {
                    UnifiedPush.tryUseCurrentOrDefaultDistributor(activity) { chosen ->
                        refresh()
                        if (chosen) scope.launch { register(key) }
                    }
                }
            }
        }
    }

    /** Let the user pick among installed distributors (Settings); [onChosen] runs before registering. */
    fun pick(activity: Activity, onChosen: suspend () -> Unit = {}) {
        UnifiedPush.tryPickDistributor(activity) { chosen ->
            refresh()
            if (chosen) {
                scope.launch {
                    onChosen()
                    currentKey()?.let { register(it) }
                }
            }
        }
    }

    /**
     * The server's key may have changed (first lookup, rotation): register
     * again, but only with a distributor already chosen; this can run in
     * the background.
     */
    suspend fun onKeyChanged() {
        val key = currentKey() ?: return
        if (UnifiedPush.getAckDistributor(context) != null) register(key)
    }

    /** The distributor handed out an endpoint for [instance]. */
    suspend fun onNewEndpoint(instance: String, url: String, p256dh: String, auth: String) {
        val state = sessionManager.pushState.first()
        // The endpoint is bound to the key the request named.
        val kid = state?.requestedKid ?: state?.advertised?.kid ?: return
        val endpoint = PushEndpoint.WebPush(url = url, p256dh = p256dh, auth = auth, vapidKid = kid)
        sessionManager.recordPushEndpoint(instance, endpoint)
        refresh()
    }

    /**
     * Sign-out (any path, including one that already wiped the session):
     * drop the account's distributor registration so its endpoint dies and
     * the server stops pushing to it.
     */
    suspend fun unregister() {
        val instance = userPrefs.lastPushInstance.first() ?: return
        runCatching { UnifiedPush.unregister(context, instance) }
        userPrefs.setLastPushInstance(null)
    }

    private suspend fun currentKey(): VapidKey? {
        val owner = sessionPrefs.ownerBareJid.first() ?: return null
        val state = sessionManager.pushState.first()?.takeIf { it.owner == owner } ?: return null
        if (state.unsupported) return null
        return state.advertised
    }

    private suspend fun register(key: VapidKey) {
        val instance = sessionManager.pushInstance() ?: return
        sessionManager.recordPushDistributorRequest(instance, key.kid)
        userPrefs.setLastPushInstance(instance)
        runCatching {
            UnifiedPush.register(
                context,
                instance,
                context.getString(R.string.push_distributor_message),
                key.publicKey,
            )
        }
    }

    private fun labelOf(packageName: String): String = runCatching {
        val pm = context.packageManager
        pm.getApplicationLabel(pm.getApplicationInfo(packageName, PackageManager.ApplicationInfoFlags.of(0))).toString()
    }.getOrDefault(packageName)
}
