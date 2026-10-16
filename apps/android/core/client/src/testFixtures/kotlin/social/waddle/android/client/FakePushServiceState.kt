package social.waddle.android.client

import kotlinx.coroutines.CompletableDeferred
import social.waddle.client.ffi.WaddlePushDeviceCredentials
import social.waddle.client.ffi.WaddlePushEnvironment
import social.waddle.client.ffi.WaddlePushVapidLookup
import social.waddle.client.ffi.WaddleRegisterDeviceResult
import java.util.concurrent.CopyOnWriteArrayList

/** One recorded XEP-0050 `register-device` call. */
data class RecordedPushRegister(
    val serviceJid: String,
    val appId: String,
    val environment: WaddlePushEnvironment,
    val credentials: WaddlePushDeviceCredentials,
)

/** One recorded `disable-device` call. */
data class RecordedPushDisable(val serviceJid: String, val node: String, val deviceId: String)

/**
 * The fake client's XEP-0357 Push Service: canned VAPID lookups and
 * register/enable/disable outcomes, every call recorded in order.
 */
class FakePushServiceState {
    @Volatile
    var vapid: WaddlePushVapidLookup =
        WaddlePushVapidLookup.Advertised(publicKey = TEST_VAPID_KEY, kid = TEST_VAPID_KID)

    val vapidLookups = CopyOnWriteArrayList<String>()
    val registers = CopyOnWriteArrayList<RecordedPushRegister>()
    val enables = CopyOnWriteArrayList<Pair<String, String>>()
    val disables = CopyOnWriteArrayList<RecordedPushDisable>()

    /** Assigned per register call: device ids `dev-1`, `dev-2`, ... on node `node-1`. */
    @Volatile
    var registerFails = false

    @Volatile
    var enableSucceeds = true

    @Volatile
    var disableSucceeds = true

    /** When set, `register` parks until completed (to race logout or endpoint changes). */
    @Volatile
    var registerGate: CompletableDeferred<Unit>? = null

    suspend fun lookup(serviceJid: String): WaddlePushVapidLookup {
        vapidLookups += serviceJid
        return vapid
    }

    suspend fun register(call: RecordedPushRegister): WaddleRegisterDeviceResult? {
        registers += call
        registerGate?.await()
        if (registerFails) return null
        return WaddleRegisterDeviceResult(node = "node-1", deviceId = "dev-${registers.size}")
    }

    fun enable(serviceJid: String, node: String): Boolean {
        enables += serviceJid to node
        return enableSucceeds
    }

    fun disable(call: RecordedPushDisable): Boolean {
        disables += call
        return disableSucceeds
    }

    companion object {
        /** base64url of a 65-byte uncompressed P-256 point. */
        val TEST_VAPID_KEY = "B" + "A".repeat(VAPID_KEY_LENGTH - 1)
        private const val VAPID_KEY_LENGTH = 87
        const val TEST_VAPID_KID = "0b3c1d7e-8f4a-4d2b-9c6e-1a2b3c4d5e6f"
    }
}
