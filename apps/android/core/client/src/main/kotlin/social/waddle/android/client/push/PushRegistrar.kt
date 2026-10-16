package social.waddle.android.client.push

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withTimeoutOrNull
import social.waddle.android.client.prefs.PushDevice
import social.waddle.android.client.prefs.PushDeviceRef
import social.waddle.android.client.prefs.PushEndpoint
import social.waddle.android.client.prefs.PushState
import social.waddle.android.client.prefs.PushStateStore
import social.waddle.android.client.prefs.VapidKey
import social.waddle.android.client.session.ActiveSession
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddlePushDeviceCredentials
import social.waddle.client.ffi.WaddlePushEnvironment
import social.waddle.client.ffi.WaddlePushVapidLookup
import java.security.MessageDigest
import java.util.Locale
import java.util.concurrent.ConcurrentHashMap

/**
 * Keeps the account's XEP-0357 Push Service registration in step with
 * the one endpoint the app holds: a UnifiedPush (Web Push provider) or an
 * FCM endpoint (iOS `PushRegistrationStore` parity). Switching transport
 * replaces the endpoint, so the old device is retired before the new one
 * registers and a push never notifies twice.
 *
 * One reconcile pass runs per ready stream and whenever the endpoint
 * changes; passes never overlap and extra triggers collapse into one
 * more pass. Passes live on the ready attempt and end with it. Each IQ
 * takes its own owner lease, and every durable write goes through the
 * same lease, so a pass that loses its session (logout,
 * a dropped stream, released demand) stops without writing anything.
 *
 * Devices are recorded the moment the service assigns them, and a device
 * that must go is queued for disabling before the attempt, so a failure
 * or cancellation at any step leaves at most a queued retry, never an
 * orphan the next pass cannot see.
 */
internal class PushRegistrar(
    private val store: PushStateStore,
    private val activeSession: ActiveSession,
) {
    private val triggers = Channel<Unit>(Channel.CONFLATED)
    private val passLock = Mutex()
    private val passes: MutableSet<Job> = ConcurrentHashMap.newKeySet()

    /** The ready attempt passes run on; replaced on every ready stream. */
    @Volatile
    private var ready: ReadyAttempt? = null

    private class ReadyAttempt(val scope: CoroutineScope, val lease: ActiveSession.OwnerLease)

    /** A stream became ready: reconcile on it now and for later triggers. */
    fun onReady(attemptScope: CoroutineScope, lease: ActiveSession.OwnerLease) {
        ready = ReadyAttempt(attemptScope, lease)
        requestReconcile()
    }

    /**
     * Something changed (endpoint, distributor): reconcile on the ready
     * stream, or on the next one. Passes are children of the attempt, so
     * an ending stream cancels them; a trigger they did not get to stays
     * queued, and every ready stream triggers a full pass anyway.
     */
    fun requestReconcile() {
        triggers.trySend(Unit)
        val attempt = ready ?: return
        val pass = attempt.scope.launch {
            passLock.withLock {
                while (triggers.tryReceive().isSuccess && activeSession.isCurrent(attempt.lease)) {
                    reconcile(attempt.lease)
                }
            }
        }
        passes += pass
        pass.invokeOnCompletion { passes -= pass }
    }

    /**
     * Sign-out, while the stream is still live: stop any pass and disable
     * this device so the server stops pushing to it. Best-effort; the
     * caller bounds it in time.
     */
    suspend fun retireForLogout(lease: ActiveSession.OwnerLease) {
        ready = null
        passes.toList().forEach { it.cancelAndJoin() }
        retireDevice(lease, lease.ownerBareJid)
    }

    private suspend fun reconcile(lease: ActiveSession.OwnerLease) {
        val owner = lease.ownerBareJid
        val service = pushServiceJidFor(owner) ?: return
        if (!retryPendingDisables(lease, owner)) return
        // FCM endpoints are not bound to the server's VAPID key, and the
        // server needs no Web Push provider to deliver to them.
        val held = store.read(owner).endpoint
        if (held is PushEndpoint.Fcm) {
            reconcileDevice(lease, owner, service, held)
            return
        }
        val key = when (val lookup = call(lease) { it.fetchPushVapidPublicKey(service) } ?: return) {
            // Transient: keep what is registered, just re-assert it.
            WaddlePushVapidLookup.Failed -> {
                refreshEnable(lease, owner)
                return
            }
            WaddlePushVapidLookup.NotAdvertised -> {
                if (!write(lease, owner) { it.copy(unsupported = true, advertised = null) }) return
                retireDevice(lease, owner)
                return
            }
            is WaddlePushVapidLookup.Advertised -> VapidKey(publicKey = lookup.publicKey, kid = lookup.kid)
        }
        if (!write(lease, owner) { it.copy(unsupported = false, advertised = key) }) return
        when (val endpoint = store.read(owner).endpoint) {
            null -> retireDevice(lease, owner)
            // The distributor still holds an endpoint for an older key;
            // the app re-registers it, and the new endpoint triggers a pass.
            is PushEndpoint.WebPush -> if (endpoint.vapidKid == key.kid) {
                reconcileDevice(lease, owner, service, endpoint)
            }
            // Switched to FCM while the key was being looked up.
            is PushEndpoint.Fcm -> reconcileDevice(lease, owner, service, endpoint)
        }
    }

    /** Keep the device registered for [endpoint], replacing any other. */
    private suspend fun reconcileDevice(
        lease: ActiveSession.OwnerLease,
        owner: String,
        service: String,
        endpoint: PushEndpoint,
    ) {
        if (store.read(owner).device?.matches(service, endpoint) == true) {
            refreshEnable(lease, owner)
            return
        }
        if (!retireDevice(lease, owner)) return
        register(lease, owner, service, endpoint)
    }

    private suspend fun register(
        lease: ActiveSession.OwnerLease,
        owner: String,
        service: String,
        endpoint: PushEndpoint,
    ) {
        val credentials = when (endpoint) {
            is PushEndpoint.WebPush -> WaddlePushDeviceCredentials.WebPush(
                endpoint = endpoint.url,
                p256dh = endpoint.p256dh,
                auth = endpoint.auth,
            )
            is PushEndpoint.Fcm -> WaddlePushDeviceCredentials.Fcm(
                registrationToken = endpoint.token,
                p256dh = endpoint.p256dh,
                auth = endpoint.auth,
            )
        }
        val outcome = call(lease) {
            Optional(it.registerPushDevice(service, APP_ID, WaddlePushEnvironment.PRODUCTION, credentials))
        } ?: return
        val result = outcome.value
        if (result == null) {
            write(lease, owner) { it.copy(failed = true) }
            return
        }
        val ref = PushDeviceRef(serviceJid = service, node = result.node, deviceId = result.deviceId)
        val device = PushDevice(ref = ref, endpoint = endpoint, enabled = false)
        // Recorded before <enable/> so nothing after this can orphan it.
        if (!write(lease, owner) { it.copy(device = device, failed = false) }) return
        val enabled = call(lease) { it.enablePushNotifications(service, result.node) } ?: return
        if (enabled) {
            write(lease, owner) { state -> state.copy(device = state.device?.copy(enabled = true)) }
        } else {
            if (!write(lease, owner) { it.copy(failed = true) }) return
            retireDevice(lease, owner)
        }
    }

    /** Re-send `<enable/>` for the current device (a cheap idempotent refresh). */
    private suspend fun refreshEnable(lease: ActiveSession.OwnerLease, owner: String) {
        val device = store.read(owner).device ?: return
        val enabled = call(lease) { it.enablePushNotifications(device.ref.serviceJid, device.ref.node) } ?: return
        if (enabled != device.enabled) {
            write(lease, owner) { state ->
                state.copy(device = state.device?.takeIf { it.ref == device.ref }?.copy(enabled = enabled))
            }
        }
    }

    /** Queue the current device for disabling, then work the queue. */
    private suspend fun retireDevice(lease: ActiveSession.OwnerLease, owner: String): Boolean {
        val device = store.read(owner).device
        if (device != null) {
            val queued = write(lease, owner) { state ->
                state.copy(
                    device = null,
                    pendingDisables = (state.pendingDisables + device.ref).distinct().takeLast(MAX_PENDING_DISABLES),
                )
            }
            if (!queued) return false
        }
        return retryPendingDisables(lease, owner)
    }

    /** Disable every queued device; `false` when the session went away. */
    private suspend fun retryPendingDisables(lease: ActiveSession.OwnerLease, owner: String): Boolean {
        for (ref in store.read(owner).pendingDisables) {
            val disabled = call(lease) { it.disablePushDevice(ref.serviceJid, ref.node, ref.deviceId) } ?: return false
            // A refusal stays queued for the next pass.
            if (disabled && !write(lease, owner) { it.copy(pendingDisables = it.pendingDisables - ref) }) return false
        }
        return true
    }

    /**
     * One FFI call under [lease]; `null` when the session is gone, the call
     * threw, or it outlived [CALL_TIMEOUT_MILLIS] (a hung IQ must neither
     * stall the pass nor hold up the attempt it runs on).
     */
    private suspend fun <T : Any> call(lease: ActiveSession.OwnerLease, op: suspend (WaddleClientInterface) -> T): T? =
        try {
            withTimeoutOrNull(CALL_TIMEOUT_MILLIS) {
                when (val invocation = activeSession.invokeIfCurrent(lease, op)) {
                    ActiveSession.LeaseInvocation.Stale,
                    ActiveSession.LeaseInvocation.NotConnected,
                    -> null
                    is ActiveSession.LeaseInvocation.Completed -> invocation.value
                }
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            null
        }

    /** Persist under [lease]; `false` (nothing written) once it is stale. */
    private suspend fun write(
        lease: ActiveSession.OwnerLease,
        owner: String,
        transform: (PushState) -> PushState,
    ): Boolean = activeSession.runIfCurrent(lease) { store.update(owner, transform) }

    /** Lets a nullable FFI result pass through [call]'s non-null contract. */
    private class Optional<T>(val value: T)

    companion object {
        /** XEP-0357 push node scope at the Push Service: one per app. */
        const val APP_ID = "social.waddle.android"

        /** Bounded so a long-unreachable service cannot grow the state forever. */
        const val MAX_PENDING_DISABLES = 16

        /** Per IQ; the XEP-0050 exchanges take a few round trips. */
        const val CALL_TIMEOUT_MILLIS = 20_000L
    }
}

private fun PushDevice.matches(service: String, endpoint: PushEndpoint): Boolean =
    ref.serviceJid == service && this.endpoint == endpoint

/** The account's Push Service: `push.<domain>` (iOS and web parity). */
fun pushServiceJidFor(ownerBareJid: String): String? =
    ownerBareJid.substringAfter('@', missingDelimiterValue = "").takeIf { it.isNotEmpty() }?.let { "push.$it" }

/**
 * The UnifiedPush instance name for an account. Scoping it to the account
 * keeps a later account from inheriting the previous one's endpoint (the
 * payload names no account), without putting the JID in the distributor.
 */
fun pushInstanceFor(ownerBareJid: String): String {
    val digest = MessageDigest.getInstance("SHA-256")
        .digest(ownerBareJid.lowercase(Locale.ROOT).toByteArray(Charsets.UTF_8))
    return "waddle-" + digest.take(INSTANCE_DIGEST_BYTES).joinToString("") { "%02x".format(it) }
}

private const val INSTANCE_DIGEST_BYTES = 8
