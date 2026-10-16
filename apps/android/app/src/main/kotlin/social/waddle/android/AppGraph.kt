package social.waddle.android

import android.content.Context
import android.net.ConnectivityManager
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import okhttp3.OkHttpClient
import social.waddle.android.client.ClientFactory
import social.waddle.android.client.ConnectivityNetworkSignal
import social.waddle.android.client.EncryptedAttachmentDownloader
import social.waddle.android.client.GifSearchGateway
import social.waddle.android.client.GiphyProxyGateway
import social.waddle.android.client.NetworkSignal
import social.waddle.android.client.RustClientFactory
import social.waddle.android.client.WaddleAppState
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.auth.WaddleAuthApi
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.calls.CallState
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.prefs.sessionPreferencesDataStore
import social.waddle.android.client.prefs.userPreferencesDataStore
import social.waddle.android.client.push.PushStatus
import social.waddle.android.client.push.pushStatusOf
import social.waddle.android.client.session.fixedRateTicks
import social.waddle.android.client.store.ConversationKind
import social.waddle.android.feature.call.CallMediaController
import social.waddle.android.feature.call.CallSessionController
import social.waddle.android.feature.call.LiveKitCallMediaController
import social.waddle.android.feature.call.MucCallLiveParticipantsStore
import social.waddle.android.feature.conversation.AttachmentUploader
import social.waddle.android.feature.conversation.EncryptedAttachmentOpener
import social.waddle.android.feature.conversation.StickerPackCreator
import social.waddle.android.feature.login.LoginAuthGateway
import social.waddle.android.feature.login.WaddleLoginAuthGateway
import social.waddle.android.jid.localpartOf
import social.waddle.android.push.AlarmWakeTimer
import social.waddle.android.push.ConnectionDemand
import social.waddle.android.push.PushDistributor
import social.waddle.android.push.PushMessageHandler
import social.waddle.android.push.needsStream
import social.waddle.android.push.pushSummaryOf
import social.waddle.android.service.CallNotifier
import social.waddle.android.service.ConnectionServiceController
import social.waddle.android.service.MessageNotifier
import social.waddle.android.service.SessionPrefsPushedItems

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
    loginGatewayFactory: (() -> LoginAuthGateway)? = null,
    gifSearchGateway: GifSearchGateway? = null,
    /**
     * Media plane behind the call engine. Tests inject a fake so the
     * hermetic suites never open a real mic/camera or WebRTC stack.
     */
    callMediaControllerFactory: ((CoroutineScope) -> CallMediaController)? = null,
    /**
     * Overrides the connection demand. Hermetic tests pass an always-on
     * flow: they drive sessions without a process lifecycle.
     */
    connectionWanted: StateFlow<Boolean>? = null,
) {
    private val appContext: Context = context.applicationContext

    /** Process-lifetime scope for restore, service control, notifications. */
    val applicationScope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    val okHttpClient: OkHttpClient = OkHttpClient()

    val sessionPrefs: SessionPrefs = SessionPrefs(sessionStore ?: appContext.sessionPreferencesDataStore)
    val userPrefs: UserPrefs = UserPrefs(userStore ?: appContext.userPreferencesDataStore)
    val authApi: WaddleAuthApi = WaddleAuthApi(serverUrl, okHttpClient)

    val loginGateway: () -> LoginAuthGateway =
        loginGatewayFactory ?: { WaddleLoginAuthGateway(authApi) }

    /**
     * GIF search over the server origin's `/api/giphy` proxy (the web
     * contract). Same-origin as the auth REST base; a deployment
     * without the proxy degrades to the "not configured" picker state.
     */
    val gifSearchGateway: GifSearchGateway =
        gifSearchGateway ?: GiphyProxyGateway(serverUrl, okHttpClient)

    private val networkSignal: NetworkSignal = networkSignal
        ?: ConnectivityNetworkSignal(appContext.getSystemService(ConnectivityManager::class.java))

    /** What needs the XMPP stream right now (see [ConnectionDemand]). */
    val connectionDemand: ConnectionDemand =
        ConnectionDemand(applicationScope, wakeTimer = AlarmWakeTimer(appContext))

    val sessionManager: XmppSessionManager = XmppSessionManager(
        sessionPrefs = sessionPrefs,
        clientFactory = clientFactory,
        networkSignal = this.networkSignal,
        userPrefs = userPrefs,
        livenessTicks = fixedRateTicks(),
        connectionWanted = connectionWanted ?: connectionDemand.wanted,
        desiredPresence = connectionDemand.presence,
    )

    /** Push delivery for the signed-in account, known from disk on a cold start. */
    val pushStatus: StateFlow<PushStatus?> =
        combine(sessionManager.pushState, sessionPrefs.ownerBareJid, ::pushStatusOf)
            .stateIn(applicationScope, SharingStarted.Eagerly, null)

    /**
     * Whether to hold the stream in the background: the user's choice, or
     * by default whenever push is not delivering. `null` until loaded.
     */
    val stayConnected: StateFlow<Boolean?> =
        // Unknown until BOTH stores have loaded: guessing from one would
        // briefly hold the stream (or start the service) in push mode.
        combine(userPrefs.stayConnected, pushStatus) { choice, status ->
            status?.let { choice ?: (it != PushStatus.ACTIVE) }
        }
            .stateIn(applicationScope, SharingStarted.Eagerly, null)

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
        stayConnected = stayConnected,
        scope = applicationScope,
        onForeground = sessionManager::onAppForeground,
    )

    val attachmentUploader: AttachmentUploader = AttachmentUploader(
        contentResolver = appContext.contentResolver,
        httpClient = okHttpClient,
        sessionManager = sessionManager,
        cacheDir = appContext.cacheDir,
    )

    /** XEP-0448 decrypt-on-display fetch (memory-only plaintext). */
    val encryptedAttachmentDownloader: EncryptedAttachmentDownloader =
        EncryptedAttachmentDownloader(okHttpClient)

    /** XEP-0448 non-image attachments: decrypt to cache + viewer intent. */
    val encryptedAttachmentOpener: EncryptedAttachmentOpener = EncryptedAttachmentOpener(
        context = appContext,
        downloader = encryptedAttachmentDownloader,
    )

    /** XEP-0449 create-pack pipeline (downscale → slot upload → publish). */
    val stickerPackCreator: StickerPackCreator = StickerPackCreator(
        contentResolver = appContext.contentResolver,
        httpClient = okHttpClient,
        sessionManager = sessionManager,
    )

    /** LiveKit-fed roster projection for the room we're in a call with. */
    val mucCallLiveParticipants: MucCallLiveParticipantsStore = MucCallLiveParticipantsStore()

    /** Call platform glue: in-call FGS + LiveKit media session. */
    val callSessionController: CallSessionController = CallSessionController(
        context = appContext,
        sessionManager = sessionManager,
        media = callMediaControllerFactory?.invoke(applicationScope)
            ?: LiveKitCallMediaController(appContext, applicationScope),
        scope = applicationScope,
        liveParticipants = mucCallLiveParticipants,
    )

    /** Incoming-call ring notification (full-screen intent + actions). */
    val callNotifier: CallNotifier = CallNotifier(
        context = appContext,
        callState = sessionManager.callStore.state,
    )

    val messageNotifier: MessageNotifier = MessageNotifier(
        context = appContext,
        events = sessionManager.events,
        userPrefs = userPrefs,
        currentSession = currentSession,
        notifyModeFor = { conversationJid, isGroupchat ->
            sessionManager.notifySettingsStore.modeFor(
                conversationJid,
                // No public/private room discriminator yet (web parity):
                // every MUC resolves as a private group (§3 default: always).
                if (isGroupchat) ConversationKind.PRIVATE_GROUP else ConversationKind.DIRECT_CHAT,
            )
        },
        pushedItems = SessionPrefsPushedItems(sessionPrefs),
    )

    val pushDistributor: PushDistributor = PushDistributor(
        context = appContext,
        scope = applicationScope,
        sessionManager = sessionManager,
        sessionPrefs = sessionPrefs,
        userPrefs = userPrefs,
    )

    val pushMessageHandler: PushMessageHandler = PushMessageHandler(
        sessionManager = sessionManager,
        userPrefs = userPrefs,
        notifier = messageNotifier,
        isAppVisible = {
            ProcessLifecycleOwner.get().lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)
        },
        summarize = { envelope -> pushSummaryOf(envelope, appContext.resources, ::conversationTitleOf) },
    )

    /** Keep the stream up until [ConnectionDemand.Lease.release] (notification actions). */
    fun holdConnection(): ConnectionDemand.Lease = connectionDemand.acquire()

    private fun conversationTitleOf(conversationJid: String, isGroupchat: Boolean): String {
        if (!isGroupchat) return localpartOf(conversationJid)
        val name = sessionManager.roomStore.topology.value.channels
            .firstOrNull { it.roomJid == conversationJid }
            ?.name
        return "#" + (name ?: localpartOf(conversationJid))
    }

    /** Feed the connection demand (and own presence) from the process, the delivery mode and calls. */
    private fun startConnectionDemand() {
        ProcessLifecycleOwner.get().lifecycle.addObserver(object : DefaultLifecycleObserver {
            override fun onStart(owner: LifecycleOwner) = connectionDemand.setProcessStarted(true)

            override fun onStop(owner: LifecycleOwner) = connectionDemand.setProcessStarted(false)
        })
        applicationScope.launch {
            stayConnected.filterNotNull().collect(connectionDemand::setStayConnected)
        }
        applicationScope.launch {
            // An ended call waits in its slot until the UI dismisses it;
            // only ringing or live calls need signalling.
            sessionManager.callStore.state.collect {
                connectionDemand.setCallActive(it.needsStream())
                // ADR-010: a live call pauses auto-away; a ring does not.
                connectionDemand.setInCall(it is CallState.Active)
            }
        }
        // A newly learned or rotated server key needs a fresh endpoint.
        applicationScope.launch {
            sessionManager.pushState
                .map { it?.advertised?.kid }
                .distinctUntilChanged()
                .filterNotNull()
                .collect { runCatching { pushDistributor.onKeyChanged() } }
        }
        pushDistributor.refresh()
    }

    /** Wire the long-lived collectors and kick off session restore. */
    fun start() {
        startConnectionDemand()
        serviceController.start()
        messageNotifier.start(applicationScope)
        callSessionController.start()
        callNotifier.start(applicationScope)
        startSignedOutCleanup()
        // Decrypted XEP-0448 attachments never survive a process
        // restart: their FileProvider grants are gone, so the
        // plaintext on disk is only liability.
        applicationScope.launch { encryptedAttachmentOpener.clearCache() }
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
                    encryptedAttachmentOpener.clearCache()
                    // Also covers a server-side sign-out that already
                    // wiped the session: the endpoint must die with it.
                    runCatching { pushDistributor.unregister() }
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
        runCatching { pushDistributor.unregister() }
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
