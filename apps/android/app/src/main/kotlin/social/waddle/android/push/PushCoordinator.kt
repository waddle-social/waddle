package social.waddle.android.push

import android.app.Activity
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withTimeoutOrNull
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.prefs.PushEndpoint
import social.waddle.android.client.prefs.PushEndpointKind
import social.waddle.android.client.prefs.PushTransportChoice
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.push.pushInstanceFor

/**
 * FCM as a build provides it: the Play build backs this with Google's FCM
 * library; the FOSS build has none.
 */
interface FcmTokens {
    /** The current registration token; `null` when FCM cannot be reached. */
    suspend fun token(): String?

    /** Delete the token so FCM stops delivering to it; `false` if that failed. */
    suspend fun delete(): Boolean

    /** Whether the FCM library keeps the token fresh in the background. */
    fun setAutoInit(enabled: Boolean)
}

/** The transport that holds the account's one push endpoint. */
enum class PushTransport { UNIFIED_PUSH, FCM }

/**
 * Picks the push transport and routes every transport callback through
 * that choice, so the account holds exactly one endpoint (and so one
 * device at the Push Service, and one notification per push).
 *
 * Without FCM (the FOSS build) this is UnifiedPush, as before. With FCM
 * (the Play build), FCM is the default and UnifiedPush is used only after
 * the user picks a distributor in Settings, while one is installed; a
 * distributor is never chosen silently. A callback from the transport not
 * in use (a late endpoint or unregistration) never touches the endpoint.
 */
class PushCoordinator(
    private val scope: CoroutineScope,
    private val sessionManager: XmppSessionManager,
    private val userPrefs: UserPrefs,
    private val distributor: PushDistributor,
    private val fcm: FcmTokens?,
    private val keyWrapper: KeyWrapper,
    private val handler: PushMessageHandler,
) {
    private val fcmLock = Mutex()

    /** Whether this build can push over FCM at all. */
    val fcmAvailable: Boolean get() = fcm != null

    suspend fun transport(): PushTransport {
        if (fcm == null) return PushTransport.UNIFIED_PUSH
        distributor.refresh()
        val choice = userPrefs.pushTransport.first()
        return if (choice == PushTransportChoice.UNIFIED_PUSH && distributor.installed.value) {
            PushTransport.UNIFIED_PUSH
        } else {
            PushTransport.FCM
        }
    }

    /**
     * Whether to keep the FCM endpoint registered: FCM is the transport,
     * or it still holds the account's endpoint because the switch to a
     * distributor has not produced one yet (a refused registration, a
     * distributor that never answers). Until it is replaced, a rotated
     * FCM token must still reach the server.
     */
    private suspend fun fcmWanted(): Boolean =
        fcm != null && (transport() == PushTransport.FCM || heldFcmEndpoint() != null)

    private suspend fun heldFcmEndpoint(): PushEndpoint.Fcm? {
        val instance = sessionManager.pushInstance() ?: return null
        return sessionManager.pushState.first()
            ?.takeIf { pushInstanceFor(it.owner) == instance }
            ?.endpoint as? PushEndpoint.Fcm
    }

    /** The app came to the foreground: (re-)register with the transport in use. */
    fun onForeground(activity: Activity) {
        scope.launch {
            if (transport() == PushTransport.UNIFIED_PUSH) distributor.onForeground(activity)
            if (fcmWanted()) runCatching { registerFcm() }
        }
    }

    /** An account signed in (or restored): FCM registers without an activity. */
    suspend fun onAccountReady() {
        if (fcmWanted()) registerFcm()
    }

    /** The server's VAPID key changed; only UnifiedPush endpoints are bound to it. */
    suspend fun onKeyChanged() {
        if (transport() == PushTransport.UNIFIED_PUSH) distributor.onKeyChanged()
    }

    /** UnifiedPush handed out an endpoint; ignored unless UnifiedPush is in use. */
    suspend fun onUnifiedPushEndpoint(instance: String, url: String, p256dh: String, auth: String) {
        if (transport() == PushTransport.UNIFIED_PUSH) distributor.onNewEndpoint(instance, url, p256dh, auth)
    }

    /** UnifiedPush dropped [instance]; only a Web Push endpoint goes with it. */
    suspend fun onUnifiedPushUnregistered(instance: String) {
        sessionManager.recordPushEndpointRevoked(instance, PushEndpointKind.WEB_PUSH)
        distributor.refresh()
    }

    /** FCM rotated the registration token. */
    suspend fun onFcmToken(token: String) {
        if (fcmWanted()) registerFcm(token)
    }

    /**
     * An FCM data message: decrypt its `payload` with this install's keys
     * and hand the envelope to [PushMessageHandler]. Anything that does not
     * decrypt for the signed-in account is dropped unseen.
     */
    suspend fun onFcmMessage(data: Map<String, String>) {
        val payload = data[FCM_PAYLOAD_KEY] ?: return
        val instance = sessionManager.pushInstance() ?: return
        val endpoint = heldFcmEndpoint() ?: return
        val plaintext = FcmEnvelopeKeys.decrypt(endpoint, payload, keyWrapper) ?: return
        handler.handle(instance, plaintext, decrypted = true)
    }

    /** Settings: push over FCM, dropping any distributor registration. */
    fun useFcm() {
        scope.launch {
            userPrefs.setPushTransport(PushTransportChoice.FCM)
            runCatching { distributor.unregister() }
            runCatching { registerFcm() }
        }
    }

    /**
     * Settings: push through a UnifiedPush distributor the user picks. The
     * choice is kept only once a distributor is actually chosen; until its
     * endpoint arrives, any FCM endpoint stays registered.
     */
    fun useUnifiedPush(activity: Activity) {
        activity.runOnUiThread {
            distributor.pick(activity) {
                if (fcm != null) userPrefs.setPushTransport(PushTransportChoice.UNIFIED_PUSH)
            }
        }
    }

    /**
     * Sign-out: kill every endpoint this install holds for the account.
     * A token that cannot be deleted now is deleted before the next one is
     * fetched, so the old account's device stops receiving pushes.
     */
    suspend fun unregister() {
        runCatching { distributor.unregister() }
        val fcm = fcm ?: return
        fcmLock.withLock {
            fcm.setAutoInit(false)
            // Bounded: sign-out must not wait on the network. A deletion
            // that does not finish now finishes before the next token.
            val deleted = withTimeoutOrNull(TOKEN_DELETE_TIMEOUT_MILLIS) { fcm.delete() } ?: false
            if (!deleted) userPrefs.setFcmTokenDeletionPending(true)
        }
    }

    /** Record this install's FCM endpoint for the signed-in account. */
    private suspend fun registerFcm(rotated: String? = null) {
        val fcm = fcm ?: return
        fcmLock.withLock {
            if (userPrefs.fcmTokenDeletionPending.first()) {
                // The pending token must die before a new one is asked for,
                // or FCM would hand the doomed token straight back.
                if (!fcm.delete()) return
                userPrefs.setFcmTokenDeletionPending(false)
            }
            val instance = sessionManager.pushInstance() ?: return
            fcm.setAutoInit(true)
            val token = rotated ?: fcm.token() ?: return
            val held = heldFcmEndpoint()
            // Keep the keys while they still open; a new install, a
            // restored backup or a lost keystore key gets fresh ones.
            val usable = held?.takeIf { keyWrapper.unwrap(it.wrappedPrivateKey) != null }
            if (usable?.token == token) return
            val endpoint = usable?.copy(token = token) ?: FcmEnvelopeKeys.generate(keyWrapper).let { keys ->
                PushEndpoint.Fcm(
                    token = token,
                    p256dh = keys.p256dh,
                    auth = keys.auth,
                    wrappedPrivateKey = keys.wrappedPrivateKey,
                )
            }
            sessionManager.recordPushEndpoint(instance, endpoint)
        }
    }

    companion object {
        /** The FCM data key the Push Service puts the ciphertext under. */
        const val FCM_PAYLOAD_KEY = "payload"

        /** How long sign-out waits for FCM to delete the token. */
        const val TOKEN_DELETE_TIMEOUT_MILLIS = 5_000L
    }
}
