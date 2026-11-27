package social.waddle.android

import android.app.Activity
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
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.SupervisorJob
<<<<<<< HEAD
import kotlinx.coroutines.flow.Flow
=======
>>>>>>> origin/main
import kotlinx.coroutines.flow.FlowCollector
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import okhttp3.OkHttpClient
import social.waddle.android.client.ClientFactory
import social.waddle.android.client.ConnectionState
import social.waddle.android.client.ConnectivityNetworkSignal
import social.waddle.android.client.EncryptedAttachmentDownloader
import social.waddle.android.client.GifSearchGateway
import social.waddle.android.client.GiphyProxyGateway
import social.waddle.android.client.NetworkSignal
import social.waddle.android.client.RustClientFactory
import social.waddle.android.client.WaddleAppState
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.account.Account
import social.waddle.android.client.account.AccountRegistry
import social.waddle.android.client.account.AccountStoreProvider
import social.waddle.android.client.account.AccountStores
import social.waddle.android.client.account.LegacySessionMigration
import social.waddle.android.client.auth.WaddleAuthApi
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.calls.CallState
import social.waddle.android.client.prefs.PushEndpointKind
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.TimelineDirection
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.prefs.accountRegistryDataStore
import social.waddle.android.client.prefs.sessionPreferencesDataStore
import social.waddle.android.client.prefs.userPreferencesDataStore
import social.waddle.android.client.push.PushStatus
import social.waddle.android.client.server.ServerDiscovery
import social.waddle.android.client.server.ServerSelection
import social.waddle.android.feature.call.CallMediaController
import social.waddle.android.feature.call.CallSessionController
import social.waddle.android.feature.call.LiveKitCallMediaController
import social.waddle.android.feature.call.MucCallLiveParticipantsStore
import social.waddle.android.feature.conversation.AttachmentUploader
import social.waddle.android.feature.conversation.EncryptedAttachmentOpener
import social.waddle.android.feature.conversation.StickerPackCreator
import social.waddle.android.feature.login.LoginAuthGateway
import social.waddle.android.feature.login.WaddleLoginAuthGateway
import social.waddle.android.flavour.Distribution
import social.waddle.android.jid.bareJidOf
import social.waddle.android.push.AlarmWakeTimer
import social.waddle.android.push.AndroidKeystoreWrapper
import social.waddle.android.push.ConnectionDemand
import social.waddle.android.push.FcmTokens
import social.waddle.android.push.HeldWakeTimer
import social.waddle.android.push.KeyWrapper
import social.waddle.android.push.PowerManagerWakeLocks
import social.waddle.android.push.PushCoordinator
import social.waddle.android.push.PushDistributor
import social.waddle.android.push.needsStream
import social.waddle.android.service.ConnectionServiceController
import social.waddle.android.service.MessageNotifier
import java.io.File

/**
 * Single manual-DI composition root, created once by [WaddleApplication].
 * Everything the UI, service, and receivers need hangs off this graph —
 * no Hilt.
 *
 * Per-account state lives in [AccountRuntime]s (one per signed-in account,
 * all connected up to [LIVE_ACCOUNT_CAP]); the graph owns what is shared
 * across them and exposes the selected account's runtime through the
 * `sessionManager`, `sessionPrefs`, `currentSession`... passthroughs, so
 * screens keep reading "the current account" without knowing there are
 * several.
 *
 * Every parameter defaults to the production wiring; instrumentation
 * tests construct a second graph with fakes (in-memory DataStores are
 * mandatory there — a second graph on the real preference delegates
 * crashes with "multiple DataStores active for the same file").
 */
@OptIn(ExperimentalCoroutinesApi::class)
class AppGraph(
    context: Context,
    clientFactory: ClientFactory = RustClientFactory(),
    /**
     * Default REST auth + session base URL (BuildConfig, `-PwaddleServerUrl`
     * override). The person can pick another server on the login screen;
     * see [serverSelection].
     */
    defaultServerUrl: String = BuildConfig.WADDLE_SERVER_URL,
    /** The pre-accounts single session file, moved into the first account once. */
    legacySessionStore: DataStore<Preferences>? = null,
    /** Where each account's session store lives; default one file per account. */
    accountStores: AccountStoreProvider? = null,
    accountRegistryStore: DataStore<Preferences>? = null,
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
    /** FCM where the build has it (Play); `null` in the FOSS build. */
    fcmTokens: FcmTokens? = Distribution.fcmTokens(),
    /**
     * Whether the build may hold the stream in the background with a
     * foreground service. The Play build relies on push instead.
     */
    val stayConnectedSupported: Boolean = Distribution.stayConnectedSupported,
    /** Seals the FCM envelope private key at rest. */
    keyWrapper: KeyWrapper = AndroidKeystoreWrapper(),
    /** GIF proxy origin, independent of the XMPP/auth server (`-PwaddleGifProxyUrl`). */
    gifProxyUrl: String = BuildConfig.WADDLE_GIF_PROXY_URL,
) {
    private val appContext: Context = context.applicationContext

    /** Process-lifetime scope for restore, service control, notifications. */
    val applicationScope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    val okHttpClient: OkHttpClient = OkHttpClient()

    private val legacySessionStore: DataStore<Preferences> =
        legacySessionStore ?: appContext.sessionPreferencesDataStore
    private val accountStores: AccountStoreProvider =
        accountStores ?: AccountStores(File(appContext.filesDir, "datastore"))
    private val accountRegistry = AccountRegistry(accountRegistryStore ?: appContext.accountRegistryDataStore)
    private val defaultOrigin: String = defaultServerUrl

    val userPrefs: UserPrefs = UserPrefs(userStore ?: appContext.userPreferencesDataStore)

    /** The server in use: the build default until a custom one is chosen. */
    val serverSelection: ServerSelection = ServerSelection(userPrefs, defaultServerUrl)

    /** Validates and resolves a typed server address (host-meta aware). */
    val serverDiscovery: ServerDiscovery = ServerDiscovery(okHttpClient)

    /**
     * REST auth client bound to one origin. Nothing holds a process-wide
     * client: each caller names the server the credential belongs to, so a
     * session id can never be presented to another origin.
     */
    fun authApiFor(origin: String): WaddleAuthApi = WaddleAuthApi(origin, okHttpClient)

    val loginGateway: () -> LoginAuthGateway =
        loginGatewayFactory ?: { WaddleLoginAuthGateway { authApiFor(serverSelection.current.value) } }

    /**
     * GIF search over the web client's `/api/giphy` proxy. The XMPP/auth
     * service does not serve this route. The API key stays in the proxy.
     */
    val gifSearchGateway: GifSearchGateway =
        gifSearchGateway ?: GiphyProxyGateway(gifProxyUrl, okHttpClient)

    private val networkSignal: NetworkSignal = networkSignal
        ?: ConnectivityNetworkSignal(appContext.getSystemService(ConnectivityManager::class.java))

    /** What needs the XMPP stream right now (see [ConnectionDemand]). */
    val connectionDemand: ConnectionDemand = ConnectionDemand(
        applicationScope,
        wakeTimer = HeldWakeTimer(
            alarm = AlarmWakeTimer(appContext),
            wakeLocks = PowerManagerWakeLocks(appContext),
            scope = applicationScope,
            // Awake until the step the alarm produced is on the wire.
            settled = { sessionManager.awaitPresenceAdvertised(connectionDemand.presenceAfterWake()) },
        ),
    )

    // ---- accounts -------------------------------------------------------

    private val accountsLock = Any()
    private val _accounts = MutableStateFlow<List<Account>>(emptyList())

    /** Every account, in the order they were added. */
    val accounts: StateFlow<List<Account>> = _accounts.asStateFlow()

    private val _runtimes = MutableStateFlow<Map<String, AccountRuntime>>(emptyMap())

    /** Live runtimes by account key. */
    val runtimes: StateFlow<Map<String, AccountRuntime>> = _runtimes.asStateFlow()

    private val _activeKey = MutableStateFlow<String?>(null)

    /** The account the UI shows; `null` while signed out. */
    val activeKey: StateFlow<String?> = _activeKey.asStateFlow()

    private val registryLoaded = MutableStateFlow(false)
<<<<<<< HEAD
    private val _addingAccount = MutableStateFlow(false)

    /** The person is adding another account: the sign-in screen shows over the signed-in shell. */
    val addingAccount: StateFlow<Boolean> = _addingAccount.asStateFlow()

    private val _loginAttempt = MutableStateFlow(0)

    /** Bumped for each sign-in screen so its view model starts fresh. */
    val loginAttempt: StateFlow<Int> = _loginAttempt.asStateFlow()

    fun beginAddAccount() {
        _loginAttempt.update { it + 1 }
        _addingAccount.value = true
    }

    fun cancelAddAccount() {
        _addingAccount.value = false
    }

    /** Point the sign-in screen at the selected account's server (signing in again after a rejection). */
    suspend fun prepareReauth() {
        _activeRuntime.value.takeIf { it !== idleRuntime }?.let { serverSelection.select(it.origin) }
    }

    /** One tile per account for the switcher: identity, connection, unread and warning state. */
    val accountTiles: StateFlow<List<AccountTile>> =
        combine(_accounts, _runtimes, _activeKey, ::Triple)
            .flatMapLatest { (accounts, runtimes, key) ->
                if (accounts.isEmpty()) {
                    flowOf(emptyList())
                } else {
                    combine(accounts.map { account -> tileFlow(account, runtimes[account.key], key) }) { it.toList() }
                }
            }
            .stateIn(applicationScope, SharingStarted.Eagerly, emptyList())

    private fun tileFlow(account: Account, runtime: AccountRuntime?, activeKey: String?): Flow<AccountTile> {
        fun tile(connection: ConnectionState, unread: Int, paused: Boolean) = AccountTile(
            key = account.key,
            label = account.label,
            jid = account.bareJid,
            connection = connection,
            unread = unread,
            needsSignIn = account.needsSignIn,
            paused = paused,
            selected = account.key == activeKey,
        )
        return if (runtime == null) {
            flowOf(tile(ConnectionState.Idle, 0, false))
        } else {
            combine(
                runtime.sessionManager.connectionState,
                runtime.sessionManager.unreadStore.counts,
                runtime.isParked,
            ) { connection, counts, paused -> tile(connection, counts.values.sum(), paused) }
        }
    }
=======
>>>>>>> origin/main
    private val lastUsed = HashMap<String, Long>()

    private val runtimeEnvironment = AccountRuntimeEnvironment(
        services = RuntimeServices(appContext, okHttpClient, clientFactory, this.networkSignal),
        userPrefs = userPrefs,
        connectionDemand = connectionDemand,
        connectionWanted = connectionWanted ?: connectionDemand.wanted,
        push = PushServices(fcmTokens, keyWrapper),
        callMediaFactory = callMediaControllerFactory ?: { scope -> LiveKitCallMediaController(appContext, scope) },
        isShowing = { key ->
            _activeKey.value == key &&
                ProcessLifecycleOwner.get().lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)
        },
    )

    /**
     * Stand-in while signed out so `graph.sessionManager` and friends always
     * resolve. Never signed in, never started: it holds no credentials.
     */
    private val idleRuntime: AccountRuntime = AccountRuntime(
        account = Account(IDLE_KEY, defaultServerUrl, "", "", 0L),
        sessionPrefs = SessionPrefs(this.accountStores.storeFor(IDLE_KEY)),
        env = runtimeEnvironment,
        parentScope = applicationScope,
        onSessionRejected = {},
    )

    private val _activeRuntime = MutableStateFlow(idleRuntime)

    /** The selected account's runtime (the idle stand-in when signed out). */
    val activeRuntime: StateFlow<AccountRuntime> = _activeRuntime.asStateFlow()

    /** The runtime for [key], or the selected one when no key is given; `null` if the account is gone. */
    fun runtimeFor(key: String?): AccountRuntime? =
        if (key == null) _activeRuntime.value.takeIf { it !== idleRuntime } else _runtimes.value[key]

    /** The runtime that has a call ringing or live, preferring the selected account's. */
    fun callRuntime(): AccountRuntime? {
        val active = _activeRuntime.value
        return active.takeIf { it !== idleRuntime && it.hasCall() }
            ?: _runtimes.value.values.firstOrNull { it.hasCall() }
    }

    private fun <T> perActive(select: (AccountRuntime) -> StateFlow<T>): StateFlow<T> =
        SwitchingStateFlow(_activeRuntime, select)

    // ---- selected-account passthroughs ----------------------------------

    val sessionManager: XmppSessionManager get() = _activeRuntime.value.sessionManager
    val sessionPrefs: SessionPrefs get() = _activeRuntime.value.sessionPrefs
    val messageNotifier: MessageNotifier get() = _activeRuntime.value.messageNotifier
    val push: PushCoordinator get() = _activeRuntime.value.push
    val pushDistributor: PushDistributor get() = _activeRuntime.value.pushDistributor
    val attachmentUploader: AttachmentUploader get() = _activeRuntime.value.attachmentUploader
    val stickerPackCreator: StickerPackCreator get() = _activeRuntime.value.stickerPackCreator
    val mucCallLiveParticipants: MucCallLiveParticipantsStore get() = _activeRuntime.value.mucCallLiveParticipants
    val callSessionController: CallSessionController get() = _activeRuntime.value.callSessionController

    /** The signed-in REST session (jid/username/avatar) of the selected account; `null` when signed out. */
    val currentSession: StateFlow<WaddleSessionInfo?> = perActive { it.currentSession }

    /**
     * Origin of the selected account; `null` when signed out. Home media
     * trust and other per-server scoping read this, never the login
     * screen's [serverSelection] (which can differ while the person is
     * adding another server).
     */
    val sessionOrigin: StateFlow<String?> = perActive { if (it === idleRuntime) signedOutOrigin else it.originState }

    private val signedOutOrigin = MutableStateFlow<String?>(null)

    /** Which transport holds the selected account's push endpoint, if any. */
    val pushEndpointKind: StateFlow<PushEndpointKind?> = perActive { it.pushEndpointKind }

    /**
     * Push delivery across accounts: known only once every account's is, and
     * [PushStatus.ACTIVE] only when all of them are. Any account push does not
     * cover keeps the stream held (see [stayConnected]).
     */
    val pushStatus: StateFlow<PushStatus?> =
        _runtimes
            .flatMapLatest { runtimes ->
                if (runtimes.isEmpty()) {
                    flowOf(null)
                } else {
                    combine(runtimes.values.map { it.pushStatus }) { statuses ->
                        when {
                            statuses.any { it == null } -> null
                            statuses.all { it == PushStatus.ACTIVE } -> PushStatus.ACTIVE
                            else -> statuses.first { it != PushStatus.ACTIVE }
                        }
                    }
                }
            }
            .stateIn(applicationScope, SharingStarted.Eagerly, null)

    /**
     * Whether to hold the stream in the background: the user's choice, or
     * by default whenever push is not delivering. `null` until loaded.
     */
    val stayConnected: StateFlow<Boolean?> =
        if (stayConnectedSupported) {
            // Unknown until BOTH stores have loaded: guessing from one would
            // briefly hold the stream (or start the service) in push mode.
            combine(userPrefs.stayConnected, pushStatus) { choice, status ->
                status?.let { choice ?: (it != PushStatus.ACTIVE) }
            }
                .stateIn(applicationScope, SharingStarted.Eagerly, null)
        } else {
            // No background connection service in this build.
            MutableStateFlow(false).asStateFlow()
        }

    /**
     * Chat or social timelines, read from the start so a conversation
     * opens straight in its layout instead of flipping once it loads.
     */
    val timelineDirection: StateFlow<TimelineDirection> =
        userPrefs.timelineDirection.stateIn(applicationScope, SharingStarted.Eagerly, TimelineDirection.CHAT)

    // ---- app shell state ------------------------------------------------

    /**
     * True only until the LOCAL reads complete: the system splash must never
     * be pinned on the session-validation network round-trip.
     */
    val splashHold: StateFlow<Boolean> = combine(
        registryLoaded,
        _accounts,
        _activeRuntime.flatMapLatest { it.bootstrap.splashHold },
    ) { loaded, accounts, restoring -> !loaded || (accounts.isNotEmpty() && restoring) }
        .stateIn(applicationScope, SharingStarted.Eagerly, true)

    /**
     * App-shell gate: no accounts or a rejected selected account is
     * SignedOut (sign-in screen); otherwise the selected account's own state.
     */
    val appState: StateFlow<WaddleAppState> = combine(
        registryLoaded,
        _accounts,
        _activeKey,
        _activeRuntime.flatMapLatest { it.bootstrap.appState },
    ) { loaded, accounts, key, runtimeState ->
        val active = accounts.firstOrNull { it.key == key }
        when {
            !loaded -> WaddleAppState.Loading
            active == null || active.needsSignIn -> WaddleAppState.SignedOut
            else -> runtimeState
        }
    }
        .stateIn(applicationScope, SharingStarted.Eagerly, WaddleAppState.Loading)

    val serviceController: ConnectionServiceController = ConnectionServiceController(
        context = appContext,
        appState = appState,
        stayConnected = stayConnected,
        scope = applicationScope,
        onForeground = { _runtimes.value.values.forEach { it.sessionManager.onAppForeground() } },
    )

    /** XEP-0448 decrypt-on-display fetch (memory-only plaintext). */
    val encryptedAttachmentDownloader: EncryptedAttachmentDownloader =
        EncryptedAttachmentDownloader(okHttpClient)

    /** XEP-0448 non-image attachments: decrypt to cache + viewer intent. */
    val encryptedAttachmentOpener: EncryptedAttachmentOpener = EncryptedAttachmentOpener(
        context = appContext,
        downloader = encryptedAttachmentDownloader,
    )

    /** Keep the stream up until [ConnectionDemand.Lease.release] (notification actions). */
    fun holdConnection(): ConnectionDemand.Lease = connectionDemand.acquire()

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
            _runtimes
                .flatMapLatest { runtimes ->
                    if (runtimes.isEmpty()) {
                        flowOf(emptyList())
                    } else {
                        combine(runtimes.values.map { it.sessionManager.callStore.state }) { it.toList() }
                    }
                }
                .collect { states ->
                    connectionDemand.setCallActive(states.any { it.needsStream() })
                    // ADR-010: a live call pauses auto-away; a ring does not.
                    connectionDemand.setInCall(states.any { it is CallState.Active })
                }
        }
        pushDistributor.refresh()
    }

    /** Wire the long-lived collectors and kick off account restore. */
    fun start() {
        startConnectionDemand()
        serviceController.start()
        // Decrypted XEP-0448 attachments never survive a process
        // restart: their FileProvider grants are gone, so the
        // plaintext on disk is only liability.
        applicationScope.launch { encryptedAttachmentOpener.clearCache() }
        applicationScope.launch { restoreAll() }
    }

    // ---- account lifecycle ----------------------------------------------

    /** Load the registry (moving a pre-accounts session in first) and restore every account. */
    private suspend fun restoreAll() {
        try {
            LegacySessionMigration.migrate(legacySessionStore, accountRegistry, accountStores, defaultOrigin)
            val stored = accountRegistry.accounts.first()
            val storedActive = accountRegistry.activeKey.first()
            synchronized(accountsLock) {
                _accounts.value = stored
                stored.forEach(::attachRuntime)
                _activeKey.value = storedActive
                selectLocked(storedActive)
            }
        } finally {
            registryLoaded.value = true
        }
        _runtimes.value.values.forEach { it.bootstrap.restore() }
    }

    /** Re-run restore for every account (the error screen's retry). */
    fun restore() {
        _runtimes.value.values.forEach { it.bootstrap.restore() }
    }

    private fun attachRuntime(account: Account): AccountRuntime = synchronized(accountsLock) {
        _runtimes.value[account.key] ?: AccountRuntime(
            account = account,
            sessionPrefs = SessionPrefs(accountStores.storeFor(account.key)),
            env = runtimeEnvironment,
            parentScope = applicationScope,
            onSessionRejected = ::markNeedsSignIn,
        ).also { runtime ->
            _runtimes.update { it + (account.key to runtime) }
            runtime.start()
        }
    }

    private fun selectLocked(key: String?) {
        _activeKey.value = key
        val runtime = key?.let { _runtimes.value[it] }
        _activeRuntime.value = runtime ?: idleRuntime
        if (key != null) lastUsed[key] = System.nanoTime()
        rebalanceLocked()
    }

    /** The newest [LIVE_ACCOUNT_CAP] accounts stay connected; the rest wait, signed in but paused. */
    private fun rebalanceLocked() {
        val ranked = _runtimes.value.values.sortedWith(
            compareByDescending<AccountRuntime> { it.key == _activeKey.value }
                .thenByDescending { lastUsed[it.key] ?: 0L },
        )
        ranked.forEachIndexed { index, runtime -> runtime.setParked(index >= LIVE_ACCOUNT_CAP) }
    }

    /** Show [key]'s account. It is already connected (or resumes if the cap had paused it). */
    suspend fun selectAccount(key: String) {
        val known = synchronized(accountsLock) {
            if (_runtimes.value.containsKey(key)) {
                selectLocked(key)
                true
            } else {
                false
            }
        }
        if (known) accountRegistry.select(key)
    }

    private fun markNeedsSignIn(key: String) {
        val changed = synchronized(accountsLock) {
            val current = _accounts.value
            if (current.none { it.key == key && !it.needsSignIn }) {
                false
            } else {
                _accounts.value = current.map { if (it.key == key) it.copy(needsSignIn = true) else it }
                true
            }
        }
        if (changed) applicationScope.launch { runCatching { accountRegistry.setNeedsSignIn(key, true) } }
    }

    /**
     * Persist + start the XMPP session for a validated REST session. The
     * account is (origin, bare JID): signing in again as an existing account
     * refreshes it in place; any other identity becomes a new account.
     */
    suspend fun signIn(session: WaddleSessionInfo, origin: String = serverSelection.current.value) {
        val account = Account.create(origin, bareJidOf(session.jid))
        val runtime = synchronized(accountsLock) {
            val existing = _accounts.value.firstOrNull { it.key == account.key }
            val merged = existing?.copy(needsSignIn = false) ?: account
            _accounts.value = if (existing == null) {
                _accounts.value + merged
            } else {
                _accounts.value.map { if (it.key == merged.key) merged else it }
            }
            attachRuntime(merged)
        }
        accountRegistry.upsertAndSelect(account)
        runtime.signIn(session, origin)
        synchronized(accountsLock) { selectLocked(account.key) }
<<<<<<< HEAD
        _addingAccount.value = false
=======
>>>>>>> origin/main
    }

    /**
     * Sign out [key] (default: the selected account) and forget it. Local
     * first: the server logout is best-effort and async, and never delays the
     * visible sign-out. The other accounts are untouched.
     */
    suspend fun signOut(key: String? = _activeKey.value) {
        val runtime = key?.let { _runtimes.value[it] } ?: return
        val sessionId = runtime.sessionPrefs.sessionId.first()
        runtime.messageNotifier.clearAll()
        runtime.sessionManager.logout()
        // After local state is gone: revoking the server session may block
        // for the full HTTP timeout and must never delay the visible
        // sign-out, nor wait on the push teardown below.
        applicationScope.launch { authApiFor(runtime.origin).logout(sessionId) }
        // The FCM token belongs to the device: delete it only with the last account.
        val lastAccount = _runtimes.value.size <= 1
        runCatching { runtime.push.unregister(deleteFcmToken = lastAccount) }
        removeAccount(runtime)
    }

    private suspend fun removeAccount(runtime: AccountRuntime) {
        val next = synchronized(accountsLock) {
            _runtimes.update { it - runtime.key }
            _accounts.update { list -> list.filterNot { it.key == runtime.key } }
            lastUsed.remove(runtime.key)
            val nextKey = if (_activeKey.value == runtime.key) _accounts.value.firstOrNull()?.key else _activeKey.value
            selectLocked(nextKey)
            nextKey
        }
        runtime.shutdown()
        accountRegistry.remove(runtime.key)
        next?.let { accountRegistry.select(it) }
        accountStores.remove(runtime.key)
<<<<<<< HEAD
        if (next == null) {
            _addingAccount.value = false
            encryptedAttachmentOpener.clearCache()
        }
=======
        if (next == null) encryptedAttachmentOpener.clearCache()
>>>>>>> origin/main
    }

    /** FCM rotated the device token: every account registers it. */
    suspend fun onFcmToken(token: String) {
        _runtimes.value.values.forEach { runCatching { it.push.onFcmToken(token) } }
    }

    /** The app came to the foreground: every account (re-)registers with its push transport. */
    fun onPushForeground(activity: Activity) {
        _runtimes.value.values.forEach { it.push.onForeground(activity) }
    }

    /** Offer one FCM data message to each account until one decrypts it (each holds its own keys). */
    suspend fun onFcmMessage(data: Map<String, String>) {
        for (runtime in _runtimes.value.values) {
            if (runtime.push.onFcmMessage(data)) return
        }
    }

    /** The account a UnifiedPush callback for [instance] belongs to, if it still exists. */
    fun runtimeForPushInstance(instance: String): AccountRuntime? =
        _runtimes.value.values.firstOrNull { it.ownsPushInstance(instance) }

    private companion object {
        /** Accounts beyond this many are paused (signed in, stream closed) until selected. */
        const val LIVE_ACCOUNT_CAP = 5

        /** Key of the signed-out stand-in runtime; never a real account key (those are 16 hex chars). */
        const val IDLE_KEY = "idle"
    }
}

<<<<<<< HEAD
/** One account in the switcher. */
data class AccountTile(
    val key: String,
    /** The server's name (its host). */
    val label: String,
    val jid: String,
    val connection: ConnectionState,
    val unread: Int,
    val needsSignIn: Boolean,
    /** The live-account cap has paused this account's stream. */
    val paused: Boolean,
    val selected: Boolean,
)

=======
>>>>>>> origin/main
/**
 * A [StateFlow] that follows whichever flow [select] picks from the current
 * [active] runtime, with a synchronous `value`: a reader right after the
 * selection changes sees the new runtime's value, not the previous one's.
 */
@OptIn(ExperimentalCoroutinesApi::class)
private class SwitchingStateFlow<T>(
    private val active: StateFlow<AccountRuntime>,
    private val select: (AccountRuntime) -> StateFlow<T>,
) : StateFlow<T> {
    override val value: T get() = select(active.value).value

    override val replayCache: List<T> get() = listOf(value)

    override suspend fun collect(collector: FlowCollector<T>): Nothing {
        active.flatMapLatest { select(it) }.collect(collector)
        error("StateFlow collection never completes")
    }
}

/** Composition-local handle to the graph, provided by [MainActivity]. */
val LocalAppGraph = staticCompositionLocalOf<AppGraph> {
    error("AppGraph is not provided")
}
