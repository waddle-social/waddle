package social.waddle.android.client.prefs

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.core.stringSetPreferencesKey
import kotlin.random.Random
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map
import kotlinx.serialization.json.Json

/**
 * Session-scoped persistence (`session.preferences_pb`), mirroring the
 * web localStorage keys: `waddle.chat.session`, `waddle.chat.sm-resume`,
 * `waddle.chat.joined-rooms`, `waddle.chat.last-seen`.
 */
class SessionPrefs(
    private val dataStore: DataStore<Preferences>,
    private val json: Json = Json { ignoreUnknownKeys = true },
) {
    val serverUrl: Flow<String?> = dataStore.data.map { it[KEY_SERVER_URL] }
    val sessionId: Flow<String?> = dataStore.data.map { it[KEY_SESSION_ID] }
    val joinedRooms: Flow<Set<String>> = dataStore.data.map { it[KEY_JOINED_ROOMS] ?: emptySet() }

    val smResume: Flow<SmResumeSnapshot?> = dataStore.data.map { prefs ->
        prefs[KEY_SM_RESUME]?.let { stored ->
            runCatching { json.decodeFromString<SmResumeSnapshot>(stored) }.getOrNull()
        }
    }

    val lastSeen: Flow<Map<String, String>> = dataStore.data.map { prefs ->
        prefs[KEY_LAST_SEEN]?.let { stored ->
            runCatching { json.decodeFromString<Map<String, String>>(stored) }.getOrNull()
        } ?: emptyMap()
    }

    suspend fun setServerUrl(url: String) {
        dataStore.edit { it[KEY_SERVER_URL] = url }
    }

    suspend fun setSessionId(sessionId: String) {
        dataStore.edit { it[KEY_SESSION_ID] = sessionId }
    }

    /** `null` clears the snapshot (explicit disconnect / resume rejected). */
    suspend fun setSmResume(snapshot: SmResumeSnapshot?) {
        dataStore.edit { prefs ->
            if (snapshot == null) prefs.remove(KEY_SM_RESUME)
            else prefs[KEY_SM_RESUME] = json.encodeToString(snapshot)
        }
    }

    suspend fun setJoinedRooms(rooms: Set<String>) {
        dataStore.edit { it[KEY_JOINED_ROOMS] = rooms }
    }

    suspend fun setLastSeen(conversationJid: String, marker: String) {
        dataStore.edit { prefs ->
            val current = prefs[KEY_LAST_SEEN]?.let { stored ->
                runCatching { json.decodeFromString<Map<String, String>>(stored) }.getOrNull()
            } ?: emptyMap()
            prefs[KEY_LAST_SEEN] = json.encodeToString(current + (conversationJid to marker))
        }
    }

    /**
     * Stable-per-install 8-hex suffix for the XMPP resource
     * (`waddle-android-<suffix>`); generated once, atomically, and kept
     * across [clear] so reinstalls — not logouts — rotate the resource.
     */
    suspend fun resourceSuffix(): String {
        var suffix = ""
        dataStore.edit { prefs ->
            suffix = prefs[KEY_RESOURCE_SUFFIX] ?: generateResourceSuffix().also {
                prefs[KEY_RESOURCE_SUFFIX] = it
            }
        }
        return suffix
    }

    /** Logout: wipe everything except the per-install resource suffix. */
    suspend fun clear() {
        dataStore.edit { prefs ->
            val suffix = prefs[KEY_RESOURCE_SUFFIX]
            prefs.clear()
            suffix?.let { prefs[KEY_RESOURCE_SUFFIX] = it }
        }
    }

    private fun generateResourceSuffix(): String =
        buildString(RESOURCE_SUFFIX_LENGTH) {
            repeat(RESOURCE_SUFFIX_LENGTH) { append(HEX_ALPHABET.random(Random)) }
        }

    private companion object {
        val KEY_SERVER_URL = stringPreferencesKey("server_url")
        val KEY_SESSION_ID = stringPreferencesKey("session_id")
        val KEY_SM_RESUME = stringPreferencesKey("sm_resume")
        val KEY_JOINED_ROOMS = stringSetPreferencesKey("joined_rooms")
        val KEY_LAST_SEEN = stringPreferencesKey("last_seen")
        val KEY_RESOURCE_SUFFIX = stringPreferencesKey("resource_suffix")

        const val RESOURCE_SUFFIX_LENGTH = 8
        const val HEX_ALPHABET = "0123456789abcdef"
    }
}
