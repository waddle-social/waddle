package social.waddle.android

import android.content.Context
import androidx.lifecycle.ViewModelStore
import androidx.lifecycle.ViewModelStoreOwner
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
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
import social.waddle.android.client.NetworkSignal
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.account.Account
import social.waddle.android.client.auth.WaddleAuthApi
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.calls.CallState
import social.waddle.android.client.prefs.PushEndpointKind
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.push.PushStatus
import social.waddle.android.client.push.pushStatusOf
import social.waddle.android.client.session.fixedRateTicks
import social.waddle.android.client.store.ConversationKind
import social.waddle.android.feature.call.CallMediaController
import social.waddle.android.feature.call.CallSessionController
import social.waddle.android.feature.call.MucCallLiveParticipantsStore
import social.waddle.android.feature.conversation.AttachmentUploader
import social.waddle.android.feature.conversation.StickerPackCreator
import social.waddle.android.jid.localpartOf
import social.waddle.android.push.ConnectionDemand
import social.waddle.android.push.FcmTokens
import social.waddle.android.push.KeyWrapper
import social.waddle.android.push.PushCoordinator
import social.waddle.android.push.PushDistributor
import social.waddle.android.push.PushMessageHandler
import social.waddle.android.push.needsStream
import social.waddle.android.push.pushSummaryOf
import social.waddle.android.service.CallNotifier
import social.waddle.android.service.MessageNotifier
import social.waddle.android.service.SessionPrefsPushedItems

/** Process-wide plumbing every account's runtime builds on. */
class RuntimeServices(
    val context: Context,
    val okHttpClient: OkHttpClient,
    val clientFactory: ClientFactory,
    val networkSignal: NetworkSignal,
)

/** Where push registrations come from: FCM where the build has it, plus the key sealing its envelope key. */
class PushServices(val fcmTokens: FcmTokens?, val keyWrapper: KeyWrapper)

/** What every [AccountRuntime] shares: process-wide services, never per-account state. */
class AccountRuntimeEnvironment(
    val services: RuntimeServices,
    val userPrefs: UserPrefs,
    val connectionDemand: ConnectionDemand,
    /** Demand the account's stream is held to (tests replace the process-driven one). */
    val connectionWanted: StateFlow<Boolean>,
    val push: PushServices,
    val callMediaFactory: (CoroutineScope) -> CallMediaController,
    /** Whether the account [key]'s own UI is on screen (the app visible and that account selected). */
    val isShowing: (key: String) -> Boolean,
)

/**
 * Everything that belongs to one signed-in account: its session store, its
 * XMPP session manager and connection loop, notifications, push
 * registration and call plumbing. The graph keeps one per account in the
 * registry and keeps them all connected (up to the live cap), so switching
 * the UI to another account never reconnects it.
 *
 * Nothing here is process-global: the same JID or room JID on two servers
 * is two accounts with separate stores, cursors, queues and notification
 * tags.
 */
class AccountRuntime(
    val account: Account,
    val sessionPrefs: SessionPrefs,
    private val env: AccountRuntimeEnvironment,
    parentScope: CoroutineScope,
    /** Called when the server rejected this account's session (needs sign-in). */
    private val onSessionRejected: (key: String) -> Unit,
) {
    val key: String get() = account.key

    /** Lives and dies with the account: cancelled when the account is removed. */
    val scope: CoroutineScope =
        CoroutineScope(SupervisorJob(parentScope.coroutineContext[Job]) + Dispatchers.Default)

    /**
     * One ViewModel store per account, held by the graph rather than the
     * activity: switching accounts swaps the owner so every screen builds
     * fresh view models over the selected account, and switching back finds
     * none of the other account's state.
     */
    val viewModelStoreOwner: ViewModelStoreOwner = object : ViewModelStoreOwner {
        override val viewModelStore: ViewModelStore = ViewModelStore()
    }

    private val parked = MutableStateFlow(false)

    /** Whether the live-account cap has paused this account's stream. */
    val isParked: StateFlow<Boolean> = parked.asStateFlow()

    private val _currentSession = MutableStateFlow<WaddleSessionInfo?>(null)

    /** The signed-in REST session (jid/username/avatar); `null` until signed in. */
    val currentSession: StateFlow<WaddleSessionInfo?> = _currentSession.asStateFlow()

    /** The origin this account belongs to; fixed for the account's life. */
    val origin: String get() = account.origin

    /** [origin] as a flow, for the graph's selected-account passthrough. */
    val originState: StateFlow<String?> = MutableStateFlow<String?>(account.origin).asStateFlow()

    private val connectionWanted: StateFlow<Boolean> =
        combine(env.connectionWanted, parked) { wanted, paused -> wanted && !paused }
            .stateIn(scope, SharingStarted.Eagerly, env.connectionWanted.value)

    val sessionManager: XmppSessionManager = XmppSessionManager(
        sessionPrefs = sessionPrefs,
        accountKey = account.key,
        clientFactory = env.services.clientFactory,
        networkSignal = env.services.networkSignal,
        userPrefs = env.userPrefs,
        livenessTicks = fixedRateTicks(),
        connectionWanted = connectionWanted,
        desiredPresence = env.connectionDemand.presence,
    )

    /** True while this account's own UI is on screen: its messages need no banner. */
    private fun isShowing(): Boolean = env.isShowing(account.key)

    val messageNotifier: MessageNotifier = MessageNotifier(
        context = env.services.context,
        accountKey = account.key,
        accountLabel = account.label,
        events = sessionManager.events,
        userPrefs = env.userPrefs,
        currentSession = currentSession,
        isAppVisible = ::isShowing,
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

    val attachmentUploader: AttachmentUploader = AttachmentUploader(
        contentResolver = env.services.context.contentResolver,
        httpClient = env.services.okHttpClient,
        sessionManager = sessionManager,
        cacheDir = env.services.context.cacheDir,
    )

    val stickerPackCreator: StickerPackCreator = StickerPackCreator(
        contentResolver = env.services.context.contentResolver,
        httpClient = env.services.okHttpClient,
        sessionManager = sessionManager,
    )

    /** LiveKit-fed roster projection for the room this account is in a call with. */
    val mucCallLiveParticipants: MucCallLiveParticipantsStore = MucCallLiveParticipantsStore()

    val callSessionController: CallSessionController = CallSessionController(
        context = env.services.context,
        sessionManager = sessionManager,
        media = env.callMediaFactory(scope),
        scope = scope,
        liveParticipants = mucCallLiveParticipants,
    )

    /** Incoming-call ring for this account (tagged, so two accounts can ring at once). */
    val callNotifier: CallNotifier = CallNotifier(
        context = env.services.context,
        accountKey = account.key,
        callState = sessionManager.callStore.state,
    )

    val pushDistributor: PushDistributor = PushDistributor(
        context = env.services.context,
        scope = scope,
        sessionManager = sessionManager,
        sessionPrefs = sessionPrefs,
    )

    val pushMessageHandler: PushMessageHandler = PushMessageHandler(
        sessionManager = sessionManager,
        userPrefs = env.userPrefs,
        notifier = messageNotifier,
        isAppVisible = ::isShowing,
        summarize = { envelope -> pushSummaryOf(envelope, env.services.context.resources, ::conversationTitleOf) },
    )

    /** Which push transport holds this account's endpoint, and its callbacks. */
    val push: PushCoordinator = PushCoordinator(
        scope = scope,
        sessionManager = sessionManager,
        userPrefs = env.userPrefs,
        distributor = pushDistributor,
        fcm = env.push.fcmTokens,
        keyWrapper = env.push.keyWrapper,
        handler = pushMessageHandler,
    )

    /** Push delivery for this account, known from disk on a cold start. */
    val pushStatus: StateFlow<PushStatus?> =
        combine(sessionManager.pushState, sessionPrefs.ownerBareJid, ::pushStatusOf)
            .stateIn(scope, SharingStarted.Eagerly, null)

    /** Which transport holds this account's push endpoint, if any. */
    val pushEndpointKind: StateFlow<PushEndpointKind?> =
        combine(sessionManager.pushState, sessionPrefs.ownerBareJid) { state, owner ->
            state?.takeIf { owner != null && it.owner == owner }?.endpoint?.kind
        }
            .stateIn(scope, SharingStarted.Eagerly, null)

    /** Cold-start restore of this account's stored session. */
    val bootstrap: SessionBootstrap = SessionBootstrap(
        authApiFor = { origin -> WaddleAuthApi(origin, env.services.okHttpClient) },
        loadStoredSession = {
            sessionPrefs.sessionId.first()?.let { StoredSession(it, account.origin) }
        },
        signIn = ::signIn,
        signOutLocally = { onSessionRejected(account.key) },
        managerAppState = sessionManager.appState,
        networkSignal = env.services.networkSignal,
        scope = scope,
    )

    /** Whether the UnifiedPush [instance] (or an FCM envelope for it) is this account's. */
    fun ownsPushInstance(instance: String): Boolean =
        sessionManager.pushInstanceOf(account.bareJid) == instance

    /** Wire this account's long-lived collectors. Not called for the signed-out placeholder. */
    fun start() {
        messageNotifier.start(scope)
        callSessionController.start()
        callNotifier.start(scope)
        // A newly learned or rotated server key needs a fresh endpoint.
        scope.launch {
            sessionManager.pushState
                .map { it?.advertised?.kid }
                .distinctUntilChanged()
                .filterNotNull()
                .collect { runCatching { push.onKeyChanged() } }
        }
        // FCM needs no activity to register: do it as soon as the account is
        // signed in, without waiting for the server's VAPID key.
        scope.launch {
            sessionPrefs.ownerBareJid
                .distinctUntilChanged()
                .filterNotNull()
                .collect { runCatching { push.onAccountReady() } }
        }
        // Terminal server-side auth failures flip the manager to SignedOut
        // from inside :core:client, which has no notifier handle.
        scope.launch {
            sessionManager.appState.collect { state ->
                if (state == social.waddle.android.client.WaddleAppState.SignedOut) {
                    messageNotifier.clearAll()
                    onSessionRejected(account.key)
                }
            }
        }
    }

    /** Persist the validated session and start (or restart) its connection loop. */
    suspend fun signIn(session: WaddleSessionInfo, origin: String = account.origin) {
        _currentSession.value = session
        sessionPrefs.setServerUrl(origin)
        sessionManager.login(session)
    }

    /** Pause or resume the stream for the live-account cap; the session stays signed in. */
    fun setParked(paused: Boolean) {
        parked.value = paused
    }

    /** Whether a call is ringing or live on this account. */
    fun hasCall(): Boolean = sessionManager.callStore.state.value.needsStream()

    /** Whether a call is live (not merely ringing) on this account. */
    fun inCall(): Boolean = sessionManager.callStore.state.value is CallState.Active

    private fun conversationTitleOf(conversationJid: String, isGroupchat: Boolean): String {
        if (!isGroupchat) return localpartOf(conversationJid)
        val name = sessionManager.roomStore.topology.value.channels
            .firstOrNull { it.roomJid == conversationJid }
            ?.name
        return "#" + (name ?: localpartOf(conversationJid))
    }

    /** The account is gone: stop everything it runs and drop its view models. */
    fun shutdown() {
        scope.cancel()
        (viewModelStoreOwner.viewModelStore).clear()
    }
}
