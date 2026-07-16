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
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.prefs.ThemeMode
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.viewModelFactoryOf

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
    session: WaddleSessionInfo?,
    private val performSignOut: suspend () -> Unit,
    sessionManager: XmppSessionManager? = null,
) : ViewModel() {
    private val accountState = SettingsUiState(
        username = session?.username,
        jid = session?.jid,
        avatarUrl = session?.avatarUrl,
    )

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
            viewModelScope.launch { _isCommunityOwner.value = manager.isCommunityOwner() }
        }
    }

    val uiState: StateFlow<SettingsUiState> = combine(
        userPrefs.theme,
        userPrefs.notificationsEnabled,
        userPrefs.messageSoundsEnabled,
        userPrefs.readReceiptsEnabled,
    ) { theme, notificationsEnabled, messageSoundsEnabled, readReceiptsEnabled ->
        accountState.copy(
            theme = theme,
            notificationsEnabled = notificationsEnabled,
            messageSoundsEnabled = messageSoundsEnabled,
            readReceiptsEnabled = readReceiptsEnabled,
        )
    }.stateIn(viewModelScope, SharingStarted.Eagerly, accountState)

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
                session = graph.currentSession.value,
                performSignOut = graph::signOut,
                sessionManager = graph.sessionManager,
            )
        }
    }
}
