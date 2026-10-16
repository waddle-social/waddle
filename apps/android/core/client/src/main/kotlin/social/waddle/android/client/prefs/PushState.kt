package social.waddle.android.client.prefs

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.serialization.DeserializationStrategy
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonContentPolymorphicSerializer
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.jsonObject

/** A Push Service VAPID key as advertised in its disco#info. */
@Serializable
data class VapidKey(val publicKey: String, val kid: String)

/**
 * Where the Push Service delivers for this install, with the RFC 8291 key
 * pair the payload is encrypted to. One transport at a time: the account
 * has exactly one endpoint, so it has exactly one device and every push
 * notifies once.
 *
 * Persisted without a type discriminator; [PushEndpointSerializer] tells
 * the shapes apart, so state written before FCM existed (a bare Web Push
 * object) still reads as [WebPush].
 */
@Serializable(with = PushEndpointSerializer::class)
sealed interface PushEndpoint {
    /** Base64url P-256 public key the payload is encrypted to. */
    val p256dh: String

    /** Base64url 16-byte RFC 8291 auth secret. */
    val auth: String

    val kind: PushEndpointKind

    /**
     * An endpoint the UnifiedPush distributor handed out, with the key
     * pair the connector holds for it and the VAPID key it was registered
     * under (endpoints are bound to one VAPID key).
     */
    @Serializable
    data class WebPush(
        val url: String,
        override val p256dh: String,
        override val auth: String,
        val vapidKid: String,
    ) : PushEndpoint {
        override val kind: PushEndpointKind get() = PushEndpointKind.WEB_PUSH
    }

    /**
     * An FCM registration token, with the key pair this install generated
     * for it. [wrappedPrivateKey] is the private key sealed by a key that
     * never leaves the platform keystore; it is opaque here.
     */
    @Serializable
    data class Fcm(
        val token: String,
        override val p256dh: String,
        override val auth: String,
        val wrappedPrivateKey: String,
    ) : PushEndpoint {
        override val kind: PushEndpointKind get() = PushEndpointKind.FCM

        // The token and the private key stay out of logs.
        override fun toString(): String = "Fcm(p256dh=$p256dh)"
    }
}

/** Which transport an endpoint belongs to. */
enum class PushEndpointKind {
    WEB_PUSH,
    FCM,
}

/** Picks the [PushEndpoint] shape from its fields; FCM endpoints carry a token. */
internal object PushEndpointSerializer : JsonContentPolymorphicSerializer<PushEndpoint>(PushEndpoint::class) {
    override fun selectDeserializer(element: JsonElement): DeserializationStrategy<PushEndpoint> =
        if ("token" in element.jsonObject) PushEndpoint.Fcm.serializer() else PushEndpoint.WebPush.serializer()
}

/** A device row at the Push Service (XEP-0050 `register-device` result). */
@Serializable
data class PushDeviceRef(val serviceJid: String, val node: String, val deviceId: String)

/** The device registered for [endpoint], and whether `<enable/>` succeeded. */
@Serializable
data class PushDevice(
    val ref: PushDeviceRef,
    val endpoint: PushEndpoint,
    val enabled: Boolean,
)

/**
 * Everything push delivery needs to survive process death, scoped to one
 * account ([owner]) and wiped with the rest of the session on logout.
 */
@Serializable
data class PushState(
    val owner: String,
    /** The key the server advertises; `null` until looked up. */
    val advertised: VapidKey? = null,
    /** The server runs no Web Push provider (FCM endpoints do not need one). */
    val unsupported: Boolean = false,
    /** The kid the app last asked the distributor to register under. */
    val requestedKid: String? = null,
    val endpoint: PushEndpoint? = null,
    val device: PushDevice? = null,
    /** Devices that must still be disabled at the server. */
    val pendingDisables: List<PushDeviceRef> = emptyList(),
    /** The last registration attempt was refused (not a transport failure). */
    val failed: Boolean = false,
)

/** [PushState] in the session DataStore; one JSON value. */
class PushStateStore(
    private val dataStore: DataStore<Preferences>,
    private val json: Json,
) {
    /** The persisted state, or `null` when nothing was ever stored. */
    val state: Flow<PushState?> = dataStore.data.map { prefs -> prefs[KEY_PUSH_STATE]?.let(::decode) }

    /** The state for [owner]; another account's leftovers read as fresh. */
    suspend fun read(owner: String): PushState =
        state.first()?.takeIf { it.owner == owner } ?: PushState(owner = owner)

    /** Atomically transform [owner]'s state (another owner's is replaced). */
    suspend fun update(owner: String, transform: (PushState) -> PushState): PushState {
        var next = PushState(owner = owner)
        dataStore.edit { prefs ->
            val current = prefs[KEY_PUSH_STATE]?.let(::decode)?.takeIf { it.owner == owner }
                ?: PushState(owner = owner)
            next = transform(current)
            prefs[KEY_PUSH_STATE] = json.encodeToString(next)
        }
        return next
    }

    private fun decode(stored: String): PushState? =
        runCatching { json.decodeFromString<PushState>(stored) }.getOrNull()

    private companion object {
        val KEY_PUSH_STATE = stringPreferencesKey("push_state")
    }
}
