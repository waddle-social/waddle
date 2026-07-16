package social.waddle.android

import android.content.Context
import android.net.ConnectivityManager
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import okhttp3.OkHttpClient
import social.waddle.android.client.ClientFactory
import social.waddle.android.client.ConnectivityNetworkSignal
import social.waddle.android.client.NetworkSignal
import social.waddle.android.client.RustClientFactory
import social.waddle.android.client.WaddleAppState
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.auth.WaddleAuthApi
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.prefs.sessionPreferencesDataStore
import social.waddle.android.client.prefs.userPreferencesDataStore
import social.waddle.android.feature.conversation.AttachmentUploader
import social.waddle.android.feature.login.LoginAuthGateway
import social.waddle.android.feature.login.WaddleLoginAuthGateway
import social.waddle.android.service.ConnectionServiceController
import social.waddle.android.service.MessageNotifier

/**
 * Single manual-DI composition root, created once by [WaddleApplication].
 * Everything the UI, service, and receivers need hangs off this graph —
 * no Hilt.
 *
 * Every parameter defaults to the production wiring; instrumentation
 * tests construct a second graph with fakes (in-memory DataStores are
 * mandatory there — a second graph on the real preference delegates
 * crashes with "multiple DataStores active for the same file").
 */
class AppGraph(
    context: Context,
    clientFactory: ClientFactory = RustClientFactory(),
    /** REST auth + session base URL (BuildConfig, `-PwaddleServerUrl` override). */
    val serverUrl: String = BuildConfig.WADDLE_SERVER_URL,
    sessionStore: DataStore<Preferences>? = null,
    userStore: DataStore<Preferences>? = null,
    networkSignal: NetworkSignal? = null,
    loginGateway: (() -> LoginAuthGateway)? = null,
) {
    private val appContext: Context = context.applicationContext

    /** Process-lifetime scope for restore, service control, notifications. */
    val applicationScope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    val okHttpClient: OkHttpClient = OkHttpClient()

    val sessionPrefs: SessionPrefs = SessionPrefs(sessionStore ?: appContext.sessionPreferencesDataStore)
    val userPrefs: UserPrefs = UserPrefs(userStore ?: appContext.userPreferencesDataStore)
    val authApi: WaddleAuthApi = WaddleAuthApi(serverUrl, okHttpClient)

    val loginGateway: () -> LoginAuthGateway =
        loginGateway ?: { WaddleLoginAuthGateway(authApi) }

    private val networkSignal: NetworkSignal = networkSignal
        ?: ConnectivityNetworkSignal(appContext.getSystemService(ConnectivityManager::class.java))

    val sessionManager: XmppSessionManager = XmppSessionManager(
        sessionPrefs = sessionPrefs,
        clientFactory = clientFactory,
        networkSignal = this.networkSignal,
        userPrefs = userPrefs,
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
        networkSignal = this.networkSignal,
        scope = applicationScope,
    )

    /** App-shell gate: manager state plus cold-start restore failures. */
    val appState: StateFlow<WaddleAppState> = bootstrap.appState

    val serviceController: ConnectionServiceController = ConnectionServiceController(
        context = appContext,
        appState = appState,
        scope = applicationScope,
    )

    val attachmentUploader: AttachmentUploader = AttachmentUploader(
        contentResolver = appContext.contentResolver,
        httpClient = okHttpClient,
        sessionManager = sessionManager,
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
        startSignedOutCleanup()
        bootstrap.restore()
    }

    /** Persist + start the XMPP session for a validated REST session. */
    suspend fun signIn(session: WaddleSessionInfo) {
        _currentSession.value = session
        sessionManager.login(session)
    }

    /**
     * Terminal server-side auth failures flip appState to SignedOut from
     * inside :core:client, which has no notifier handle — without this
     * observer, stale MessagingStyle notifications (and their live reply
     * intents) survive into the next account's session.
     */
    fun startSignedOutCleanup() {
        applicationScope.launch {
            sessionManager.appState.collect { state ->
                if (state == WaddleAppState.SignedOut) {
                    messageNotifier.clearAll()
                }
            }
        }
    }

    /** Local-first sign-out; the server logout is best-effort async. */
    suspend fun signOut() {
        val sessionId = sessionPrefs.sessionId.first()
        messageNotifier.clearAll()
        _currentSession.value = null
        sessionManager.logout()
        // After local state is gone: revoking the server session may block
        // for the full HTTP timeout and must never delay the visible
        // sign-out.
        applicationScope.launch { authApi.logout(sessionId) }
    }
}

/** Composition-local handle to the graph, provided by [MainActivity]. */
val LocalAppGraph = staticCompositionLocalOf<AppGraph> {
    error("AppGraph is not provided")
}
