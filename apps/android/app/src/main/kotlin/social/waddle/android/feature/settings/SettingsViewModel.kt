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
import social.waddle.android.client.normalizedBareJid
import social.waddle.android.client.prefs.PushEndpointKind
import social.waddle.android.client.prefs.ThemeMode
import social.waddle.android.client.prefs.TimelineDirection
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.push.PushStatus
import social.waddle.android.viewModelFactoryOf
import social.waddle.client.ffi.WaddleAvatar

data class SettingsUiState(
    val username: String? = null,
    val jid: String? = null,
    val avatarUrl: String? = null,
    val theme: ThemeMode = ThemeMode.SYSTEM,
    /** Newest message at the bottom (chat) or the top (social). */
    val timelineDirection: TimelineDirection = TimelineDirection.CHAT,
    val notificationsEnabled: Boolean = true,
    val messageSoundsEnabled: Boolean = true,
    val readReceiptsEnabled: Boolean = true,
    /** Material You dynamic colour (off: Huddle's own colours). */
    val wallpaperColors: Boolean = false,
)

/** How background delivery works for the account right now. */
data class DeliveryUiState(
    val status: PushStatus = PushStatus.NONE,
    /** The chosen UnifiedPush distributor's name, if any. */
    val distributorLabel: String? = null,
    /** Whether any distributor app is installed. */
    val distributorInstalled: Boolean = false,
    /** Effective stay-connected mode (choice, else automatic). */
    val stayConnected: Boolean = true,
    /** The build can hold a background connection (not the Play build). */
    val stayConnectedSupported: Boolean = true,
    /** The build can push over FCM (the Play build). */
    val fcmAvailable: Boolean = false,
    /** Which transport holds the endpoint, if any. */
    val endpointKind: PushEndpointKind? = null,
) {
    /** The mode the app would pick without a choice. */
    val automaticStayConnected: Boolean get() = status != PushStatus.ACTIVE
}

/** The push side of Settings, from the graph (absent in plain unit tests). */
class DeliverySource(
    /** `null` until loaded. */
    val pushStatus: StateFlow<PushStatus?>,
    val distributorLabel: StateFlow<String?>,
    val distributorInstalled: StateFlow<Boolean>,
    val stayConnected: StateFlow<Boolean?>,
    val endpointKind: StateFlow<PushEndpointKind?>,
    val stayConnectedSupported: Boolean,
    val fcmAvailable: Boolean,
)

/** Account row, theme mode, notification prefs, background delivery, logout. */
class SettingsViewModel(
    private val userPrefs: UserPrefs,
    private val currentSession: StateFlow<WaddleSessionInfo?>,
    private val performSignOut: suspend () -> Unit,
    sessionManager: XmppSessionManager? = null,
    delivery: DeliverySource? = null,
) : ViewModel() {
    val deliveryState: StateFlow<DeliveryUiState> = when (delivery) {
        null -> MutableStateFlow(DeliveryUiState())
        else -> combine(
            delivery.pushStatus,
            delivery.distributorLabel,
            delivery.distributorInstalled,
            delivery.stayConnected,
            delivery.endpointKind,
        ) { loaded, label, installed, stay, kind ->
            val status = loaded ?: PushStatus.NONE
            DeliveryUiState(
                status = status,
                distributorLabel = label,
                distributorInstalled = installed,
                stayConnected = stay ?: (status != PushStatus.ACTIVE),
                stayConnectedSupported = delivery.stayConnectedSupported,
                fcmAvailable = delivery.fcmAvailable,
                endpointKind = kind,
            )
        }.stateIn(viewModelScope, SharingStarted.Eagerly, DeliveryUiState())
    }

    /**
     * An explicit stay-connected choice; choosing what the automatic mode
     * would pick anyway stores no choice, so it keeps following push.
     */
    fun setStayConnected(stay: Boolean) {
        val automatic = deliveryState.value.automaticStayConnected
        viewModelScope.launch { userPrefs.setStayConnected(if (stay == automatic) null else stay) }
    }

    /** The CURRENT account's XMPP-published avatar (derived from the
     *  live session flow, never frozen at construction); wins over the
     *  REST URL. */
    val selfAvatar: StateFlow<WaddleAvatar?> = when (sessionManager) {
        null -> MutableStateFlow(null)
        else -> combine(
            currentSession,
            sessionManager.profileStore.avatars,
        ) { session, avatars ->
            session?.jid?.let(::normalizedBareJid)?.let(avatars::get)
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
        combine(userPrefs.theme, userPrefs.wallpaperColors, userPrefs.timelineDirection, ::Triple),
        userPrefs.notificationsEnabled,
        userPrefs.messageSoundsEnabled,
        userPrefs.readReceiptsEnabled,
    ) { session, appearance, notificationsEnabled, messageSoundsEnabled, readReceiptsEnabled ->
        val (theme, wallpaperColors, timelineDirection) = appearance
        SettingsUiState(
            username = session?.username,
            jid = session?.jid,
            avatarUrl = session?.avatarUrl,
            theme = theme,
            timelineDirection = timelineDirection,
            notificationsEnabled = notificationsEnabled,
            messageSoundsEnabled = messageSoundsEnabled,
            readReceiptsEnabled = readReceiptsEnabled,
            wallpaperColors = wallpaperColors,
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

    fun setTimelineDirection(direction: TimelineDirection) {
        viewModelScope.launch { userPrefs.setTimelineDirection(direction) }
    }

    fun setWallpaperColors(enabled: Boolean) {
        viewModelScope.launch { userPrefs.setWallpaperColors(enabled) }
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
                delivery = DeliverySource(
                    pushStatus = graph.pushStatus,
                    distributorLabel = graph.pushDistributor.distributorLabel,
                    distributorInstalled = graph.pushDistributor.installed,
                    stayConnected = graph.stayConnected,
                    endpointKind = graph.pushEndpointKind,
                    stayConnectedSupported = graph.stayConnectedSupported,
                    fcmAvailable = graph.push.fcmAvailable,
                ),
            )
        }
    }
}
