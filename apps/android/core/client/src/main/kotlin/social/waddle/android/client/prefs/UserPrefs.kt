package social.waddle.android.client.prefs

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map

/** Theme mode pref, mirroring web `waddle:theme` (`light|system|dark`). */
enum class ThemeMode(val storageValue: String) {
    LIGHT("light"),
    SYSTEM("system"),
    DARK("dark"),
    ;

    companion object {
        fun fromStorage(value: String?): ThemeMode =
            entries.firstOrNull { it.storageValue == value } ?: SYSTEM
    }
}

/**
 * User-scoped settings (`user.preferences_pb`), mirroring the web keys
 * `waddle:theme`, `waddle.chat.notifications-enabled`, and
 * `waddle.chat.message-sounds-enabled`.
 */
class UserPrefs(private val dataStore: DataStore<Preferences>) {
    val theme: Flow<ThemeMode> = dataStore.data.map { ThemeMode.fromStorage(it[KEY_THEME]) }
    val notificationsEnabled: Flow<Boolean> = dataStore.data.map { it[KEY_NOTIFICATIONS] ?: true }
    val messageSoundsEnabled: Flow<Boolean> = dataStore.data.map { it[KEY_MESSAGE_SOUNDS] ?: true }

    /** XEP-0333 outbound gate, web `waddle:read-receipts` (default send). */
    val readReceiptsEnabled: Flow<Boolean> = dataStore.data.map { it[KEY_READ_RECEIPTS] ?: true }

    suspend fun setTheme(mode: ThemeMode) {
        dataStore.edit { it[KEY_THEME] = mode.storageValue }
    }

    suspend fun setNotificationsEnabled(enabled: Boolean) {
        dataStore.edit { it[KEY_NOTIFICATIONS] = enabled }
    }

    suspend fun setMessageSoundsEnabled(enabled: Boolean) {
        dataStore.edit { it[KEY_MESSAGE_SOUNDS] = enabled }
    }

    suspend fun setReadReceiptsEnabled(enabled: Boolean) {
        dataStore.edit { it[KEY_READ_RECEIPTS] = enabled }
    }

    suspend fun clear() {
        dataStore.edit { it.clear() }
    }

    private companion object {
        val KEY_THEME = stringPreferencesKey("theme")
        val KEY_NOTIFICATIONS = booleanPreferencesKey("notifications_enabled")
        val KEY_MESSAGE_SOUNDS = booleanPreferencesKey("message_sounds_enabled")
        val KEY_READ_RECEIPTS = booleanPreferencesKey("read_receipts_enabled")
    }
}
