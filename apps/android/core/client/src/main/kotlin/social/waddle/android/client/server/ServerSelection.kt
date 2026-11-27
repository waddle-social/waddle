package social.waddle.android.client.server

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import social.waddle.android.client.prefs.UserPrefs

/**
 * The Waddle server the app talks to: the build default until the person
 * picks another one before login. Auth REST calls and, through the
 * session they return, the XMPP WebSocket all follow [current].
 *
 * Deliberately one origin for now. The persisted shape (one string
 * key) is the single place to grow into a per-account server list.
 */
class ServerSelection(
    private val userPrefs: UserPrefs,
    val default: String,
) {
    private val _current = MutableStateFlow(default)

    /** The origin (`https://host[:port]`) in use; never blank. */
    val current: StateFlow<String> = _current.asStateFlow()

    val isCustom: Boolean get() = _current.value != default

    /**
     * Reads the persisted choice; call before the first request (session
     * restore). A stored session's own origin wins: it is the server that
     * issued that session id.
     */
    suspend fun load(sessionOrigin: String? = null) {
        val stored = sessionOrigin?.takeIf { it.isNotBlank() }
            ?: userPrefs.serverUrl.first()?.takeIf { it.isNotBlank() }
        stored?.let { _current.value = it }
    }

    /** Persists then switches to a validated origin; the default is stored as "no override". */
    suspend fun select(origin: String) {
        userPrefs.setServerUrl(origin.takeIf { it != default })
        _current.value = origin
    }

    suspend fun reset() = select(default)
}
