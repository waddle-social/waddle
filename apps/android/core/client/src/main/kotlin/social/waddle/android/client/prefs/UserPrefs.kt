package social.waddle.android.client.prefs

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map

/**
 * Theme: follow the phone's dark mode (the default), or pin one of
 * Huddle's two themes. Stored as `system`, `dark` and `light` (web
 * `waddle:theme`'s spellings); nothing stored reads as system.
 */
enum class ThemeMode(val storageValue: String) {
    SYSTEM("system"),
    NIGHT("dark"),
    DAYLIGHT("light"),
    ;

    /** Whether this mode draws night, given the phone's own dark mode. */
    fun isNight(systemDark: Boolean): Boolean = when (this) {
        SYSTEM -> systemDark
        NIGHT -> true
        DAYLIGHT -> false
    }

    companion object {
        fun fromStorage(value: String?): ThemeMode =
            entries.firstOrNull { it.storageValue == value } ?: SYSTEM
    }
}

/**
 * Which way conversation timelines run, mirroring web
 * `waddle:scroll-direction`: CHAT keeps the newest message at the bottom
 * above the composer; SOCIAL flips the timeline so the newest is first,
 * under a composer pinned to the top.
 */
enum class TimelineDirection(val storageValue: String) {
    CHAT("chat"),
    SOCIAL("social"),
    ;

    companion object {
        fun fromStorage(value: String?): TimelineDirection =
            entries.firstOrNull { it.storageValue == value } ?: CHAT
    }
}

/**
 * User-scoped settings (`user.preferences_pb`), mirroring the web keys
 * `waddle:theme`, `waddle:scroll-direction`, `waddle.chat.notifications-enabled`, and
 * `waddle.chat.message-sounds-enabled`.
 */
class UserPrefs(private val dataStore: DataStore<Preferences>) {
    val theme: Flow<ThemeMode> = dataStore.data.map { ThemeMode.fromStorage(it[KEY_THEME]) }

    /** Newest at the bottom (chat, the default) or at the top (social). */
    val timelineDirection: Flow<TimelineDirection> =
        dataStore.data.map { TimelineDirection.fromStorage(it[KEY_TIMELINE_DIRECTION]) }
    val notificationsEnabled: Flow<Boolean> = dataStore.data.map { it[KEY_NOTIFICATIONS] ?: true }
    val messageSoundsEnabled: Flow<Boolean> = dataStore.data.map { it[KEY_MESSAGE_SOUNDS] ?: true }

    /** XEP-0333 outbound gate, web `waddle:read-receipts` (default send). */
    val readReceiptsEnabled: Flow<Boolean> = dataStore.data.map { it[KEY_READ_RECEIPTS] ?: true }

    /**
     * Material You: take action and surface colours from the wallpaper
     * (Android dynamic colour). Off by default: Huddle's teal and ember
     * are the product's colours, and live things stay ember either way.
     */
    val wallpaperColors: Flow<Boolean> = dataStore.data.map { it[KEY_WALLPAPER_COLORS] ?: false }

    /**
     * Background delivery override: `true` keeps the stream up in a
     * foreground service, `false` relies on push alone, `null` (default)
     * decides by whether push delivery is active.
     */
    val stayConnected: Flow<Boolean?> = dataStore.data.map { it[KEY_STAY_CONNECTED] }

    /**
     * The UnifiedPush instance last registered with a distributor. Kept
     * across sign-out (unlike the session store) so a sign-out that wiped
     * the session can still unregister it.
     */
    val lastPushInstance: Flow<String?> = dataStore.data.map { it[KEY_LAST_PUSH_INSTANCE] }

    /**
     * Which push transport the user chose where more than one exists (the
     * Play build: FCM or a UnifiedPush distributor). Kept across sign-out,
     * like the distributor choice it stands for; `null` until chosen.
     */
    val pushTransport: Flow<PushTransportChoice?> =
        dataStore.data.map { prefs -> prefs[KEY_PUSH_TRANSPORT]?.let(PushTransportChoice::fromStorage) }

    /**
     * An FCM token from a signed-out account is still to be deleted (the
     * deletion needs the network). Kept across sign-out on purpose: until
     * it is deleted, the old account's device keeps getting pushes.
     */
    val fcmTokenDeletionPending: Flow<Boolean> = dataStore.data.map { it[KEY_FCM_TOKEN_DELETION_PENDING] ?: false }

    suspend fun setPushTransport(choice: PushTransportChoice) {
        dataStore.edit { it[KEY_PUSH_TRANSPORT] = choice.storageValue }
    }

    suspend fun setFcmTokenDeletionPending(pending: Boolean) {
        dataStore.edit { prefs ->
            if (pending) prefs[KEY_FCM_TOKEN_DELETION_PENDING] = true else prefs.remove(KEY_FCM_TOKEN_DELETION_PENDING)
        }
    }

    suspend fun setStayConnected(stayConnected: Boolean?) {
        dataStore.edit { prefs ->
            if (stayConnected == null) prefs.remove(KEY_STAY_CONNECTED) else prefs[KEY_STAY_CONNECTED] = stayConnected
        }
    }

    suspend fun setLastPushInstance(instance: String?) {
        dataStore.edit { prefs ->
            if (instance == null) prefs.remove(KEY_LAST_PUSH_INSTANCE) else prefs[KEY_LAST_PUSH_INSTANCE] = instance
        }
    }

    suspend fun setTheme(mode: ThemeMode) {
        dataStore.edit { it[KEY_THEME] = mode.storageValue }
    }

    suspend fun setTimelineDirection(direction: TimelineDirection) {
        dataStore.edit { it[KEY_TIMELINE_DIRECTION] = direction.storageValue }
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

    suspend fun setWallpaperColors(enabled: Boolean) {
        dataStore.edit { it[KEY_WALLPAPER_COLORS] = enabled }
    }

    suspend fun clear() {
        dataStore.edit { it.clear() }
    }

    private companion object {
        val KEY_THEME = stringPreferencesKey("theme")
        val KEY_TIMELINE_DIRECTION = stringPreferencesKey("timeline_direction")
        val KEY_NOTIFICATIONS = booleanPreferencesKey("notifications_enabled")
        val KEY_MESSAGE_SOUNDS = booleanPreferencesKey("message_sounds_enabled")
        val KEY_READ_RECEIPTS = booleanPreferencesKey("read_receipts_enabled")
        val KEY_STAY_CONNECTED = booleanPreferencesKey("stay_connected")
        val KEY_WALLPAPER_COLORS = booleanPreferencesKey("wallpaper_colors")
        val KEY_LAST_PUSH_INSTANCE = stringPreferencesKey("last_push_instance")
        val KEY_PUSH_TRANSPORT = stringPreferencesKey("push_transport")
        val KEY_FCM_TOKEN_DELETION_PENDING = booleanPreferencesKey("fcm_token_deletion_pending")
    }
}

/** A push transport the user can choose (see [UserPrefs.pushTransport]). */
enum class PushTransportChoice(val storageValue: String) {
    FCM("fcm"),
    UNIFIED_PUSH("unified_push"),
    ;

    companion object {
        fun fromStorage(value: String): PushTransportChoice? = entries.firstOrNull { it.storageValue == value }
    }
}
