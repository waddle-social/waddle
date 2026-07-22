package social.waddle.android.feature.settings

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import social.waddle.android.AppGraph
import social.waddle.android.client.ConnectionState
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.prefs.ThemeMode
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.jid.bareJidOf
import social.waddle.android.viewModelFactoryOf
import social.waddle.client.ffi.WaddleAvatar

data class SettingsUiState(
    val username: String? = null,
    val jid: String? = null,
    val avatarUrl: String? = null,
    val theme: ThemeMode = ThemeMode.SYSTEM,
    val notificationsEnabled: Boolean = true,
    val messageSoundsEnabled: Boolean = true,
    val readReceiptsEnabled: Boolean = true,
)

/** Account row, theme mode, notification prefs, logout. */
class SettingsViewModel(
    private val userPrefs: UserPrefs,
    private val currentSession: StateFlow<WaddleSessionInfo?>,
    private val performSignOut: suspend () -> Unit,
    sessionManager: XmppSessionManager? = null,
) : ViewModel() {
    /** The CURRENT account's XMPP-published avatar (derived from the
     *  live session flow, never frozen at construction); wins over the
     *  REST URL. */
    val selfAvatar: StateFlow<WaddleAvatar?> = when (sessionManager) {
        null -> MutableStateFlow(null)
        else -> combine(
            currentSession,
            sessionManager.profileStore.avatars,
        ) { session, avatars ->
            session?.jid?.let(::bareJidOf)?.let(avatars::get)
        }.stateIn(viewModelScope, SharingStarted.Eagerly, null)
    }

    private val _isCommunityOwner = MutableStateFlow(false)

    /**
     * Best-effort community-owner probe gating the admin section
     * entry (web `AdminView` parity: any failure hides the surface).
     * Not an authorization boundary — every admin command is
     * re-checked server-side.
     */
    val isCommunityOwner: StateFlow<Boolean> = _isCommunityOwner.asStateFlow()

    init {
        sessionManager?.let { manager ->
            viewModelScope.launch {
                // Probe on every Ready transition until it succeeds: an
                // early probe would race the connect and stick false,
                // and a transient IQ failure gets retried on the next
                // reconnect instead of hiding the entry forever.
                manager.connectionState.collect { state ->
                    if (state is ConnectionState.Ready && !_isCommunityOwner.value) {
                        _isCommunityOwner.value = manager.isCommunityOwner()
                    }
                }
            }
        }
    }

    val uiState: StateFlow<SettingsUiState> = combine(
        currentSession,
        userPrefs.theme,
        userPrefs.notificationsEnabled,
        userPrefs.messageSoundsEnabled,
        userPrefs.readReceiptsEnabled,
    ) { session, theme, notificationsEnabled, messageSoundsEnabled, readReceiptsEnabled ->
        SettingsUiState(
            username = session?.username,
            jid = session?.jid,
            avatarUrl = session?.avatarUrl,
            theme = theme,
            notificationsEnabled = notificationsEnabled,
            messageSoundsEnabled = messageSoundsEnabled,
            readReceiptsEnabled = readReceiptsEnabled,
        )
    }.stateIn(viewModelScope, SharingStarted.Eagerly, accountSeed())

    private fun accountSeed(): SettingsUiState {
        val session = currentSession.value
        return SettingsUiState(
            username = session?.username,
            jid = session?.jid,
            avatarUrl = session?.avatarUrl,
        )
    }

    fun setTheme(mode: ThemeMode) {
        viewModelScope.launch { userPrefs.setTheme(mode) }
    }

    fun setNotificationsEnabled(enabled: Boolean) {
        viewModelScope.launch { userPrefs.setNotificationsEnabled(enabled) }
    }

    fun setMessageSoundsEnabled(enabled: Boolean) {
        viewModelScope.launch { userPrefs.setMessageSoundsEnabled(enabled) }
    }

    fun setReadReceiptsEnabled(enabled: Boolean) {
        viewModelScope.launch { userPrefs.setReadReceiptsEnabled(enabled) }
    }

    /** Server logout + local sign-out; the app shell flips to Login. */
    fun logout() {
        viewModelScope.launch { performSignOut() }
    }

    companion object {
        fun factory(graph: AppGraph): ViewModelProvider.Factory = viewModelFactoryOf {
            SettingsViewModel(
                userPrefs = graph.userPrefs,
                currentSession = graph.currentSession,
                performSignOut = graph::signOut,
                sessionManager = graph.sessionManager,
            )
        }
    }
}
