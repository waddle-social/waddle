package social.waddle.android.client.prefs

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/** A Push Service VAPID key as advertised in its disco#info. */
@Serializable
data class VapidKey(val publicKey: String, val kid: String)

/**
 * The push endpoint the UnifiedPush distributor handed out, with the Web
 * Push key pair the connector holds for it and the VAPID key it was
 * registered under (endpoints are bound to one VAPID key).
 */
@Serializable
data class PushEndpoint(
    val url: String,
    val p256dh: String,
    val auth: String,
    val vapidKid: String,
)

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
    /** The server runs no Web Push provider. */
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
