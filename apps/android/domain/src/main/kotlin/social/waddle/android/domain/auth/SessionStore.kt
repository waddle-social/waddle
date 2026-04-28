package social.waddle.android.domain.auth

import android.content.Context
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map

private val Context.dataStore: DataStore<Preferences> by preferencesDataStore(name = "waddle_session")

/**
 * Persists the chosen server URL and the most-recent session id across
 * process restarts. Mirrors the iOS `AppConfig.persistedServerURL` /
 * `storedSessionID` pair, but folded into one Android-typical
 * Preferences DataStore.
 */
public class SessionStore(context: Context) {
    private val store = context.applicationContext.dataStore

    public val serverUrl: Flow<String> = store.data.map { it[KEY_SERVER_URL] ?: DEFAULT_SERVER_URL }

    public val sessionId: Flow<String?> = store.data.map { it[KEY_SESSION_ID] }

    public suspend fun currentServerUrl(): String = serverUrl.first()

    public suspend fun currentSessionId(): String? = sessionId.first()

    public suspend fun saveServerUrl(url: String) {
        store.edit { prefs -> prefs[KEY_SERVER_URL] = url }
    }

    public suspend fun saveSessionId(id: String) {
        store.edit { prefs -> prefs[KEY_SESSION_ID] = id }
    }

    public suspend fun clearSessionId() {
        store.edit { prefs -> prefs.remove(KEY_SESSION_ID) }
    }

    public companion object {
        public const val DEFAULT_SERVER_URL: String = "https://xmpp.waddle.social"

        private val KEY_SERVER_URL = stringPreferencesKey("server_url")
        private val KEY_SESSION_ID = stringPreferencesKey("session_id")
    }
}
