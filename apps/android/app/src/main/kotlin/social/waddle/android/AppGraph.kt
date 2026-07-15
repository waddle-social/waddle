package social.waddle.android

import android.content.Context
import android.net.ConnectivityManager
import androidx.compose.runtime.staticCompositionLocalOf
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import okhttp3.OkHttpClient
import social.waddle.android.client.ConnectivityNetworkSignal
import social.waddle.android.client.RustClientFactory
import social.waddle.android.client.WaddleAppState
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.auth.WaddleAuthApi
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.prefs.sessionPreferencesDataStore
import social.waddle.android.client.prefs.userPreferencesDataStore
import social.waddle.android.service.ConnectionServiceController
import social.waddle.android.service.MessageNotifier

/**
 * Single manual-DI composition root, created once by [WaddleApplication].
 * Everything the UI, service, and receivers need hangs off this graph —
 * no Hilt.
 */
class AppGraph(context: Context) {
    private val appContext: Context = context.applicationContext

    /** Process-lifetime scope for restore, service control, notifications. */
    val applicationScope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    val okHttpClient: OkHttpClient = OkHttpClient()

    /** REST auth + session base URL (BuildConfig, `-PwaddleServerUrl` override). */
    val serverUrl: String = BuildConfig.WADDLE_SERVER_URL

    val sessionPrefs: SessionPrefs = SessionPrefs(appContext.sessionPreferencesDataStore)
    val userPrefs: UserPrefs = UserPrefs(appContext.userPreferencesDataStore)
    val authApi: WaddleAuthApi = WaddleAuthApi(serverUrl, okHttpClient)

    val sessionManager: XmppSessionManager = XmppSessionManager(
        sessionPrefs = sessionPrefs,
        clientFactory = RustClientFactory(),
        networkSignal = ConnectivityNetworkSignal(
            appContext.getSystemService(ConnectivityManager::class.java),
        ),
    )

    private val _currentSession = MutableStateFlow<WaddleSessionInfo?>(null)

    /** The signed-in REST session (jid/username/avatar); `null` when signed out. */
    val currentSession: StateFlow<WaddleSessionInfo?> = _currentSession.asStateFlow()

    val bootstrap: SessionBootstrap = SessionBootstrap(
        sessionPrefs = sessionPrefs,
        authApi = authApi,
        signIn = ::signIn,
        signOutLocally = { sessionManager.logout() },
        managerAppState = sessionManager.appState,
        scope = applicationScope,
    )

    /** App-shell gate: manager state plus cold-start restore failures. */
    val appState: StateFlow<WaddleAppState> = bootstrap.appState

    val serviceController: ConnectionServiceController = ConnectionServiceController(
        context = appContext,
        appState = appState,
        scope = applicationScope,
    )

    val messageNotifier: MessageNotifier = MessageNotifier(
        context = appContext,
        events = sessionManager.events,
        userPrefs = userPrefs,
        currentSession = currentSession,
    )

    /** Wire the long-lived collectors and kick off session restore. */
    fun start() {
        serviceController.start()
        messageNotifier.start(applicationScope)
        bootstrap.restore()
    }

    /** Persist + start the XMPP session for a validated REST session. */
    suspend fun signIn(session: WaddleSessionInfo) {
        _currentSession.value = session
        sessionManager.login(session)
    }

    /** Server-side logout (best effort) plus full local sign-out. */
    suspend fun signOut() {
        messageNotifier.clearAll()
        authApi.logout(sessionPrefs.sessionId.first())
        _currentSession.value = null
        sessionManager.logout()
    }
}

/** Composition-local handle to the graph, provided by [MainActivity]. */
val LocalAppGraph = staticCompositionLocalOf<AppGraph> {
    error("AppGraph is not provided")
}
