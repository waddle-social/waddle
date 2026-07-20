package social.waddle.android.client.session

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.merge
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.withTimeoutOrNull
import social.waddle.android.client.ConnectionState
import social.waddle.android.client.NetworkSignal
import social.waddle.android.client.ReconnectPolicy
import social.waddle.android.client.SaslRetryDisposition
import social.waddle.android.client.SessionLifecycleRef
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.toFfi

/** Configuration owned by the supervised reconnection loop. */
internal data class ConnectionLoopConfiguration(
    val onReady: SessionReadyListener,
    val onAuthenticationStopped: suspend (
        SessionLifecycleRef,
        SaslRetryDisposition,
    ) -> Unit,
    val reconnectPolicy: ReconnectPolicy = ReconnectPolicy(),
    val connectTimeoutMillis: Long = ConnectionLoop.CONNECT_TIMEOUT_MILLIS,
)

/**
 * The sole factory boundary for an attempt-local UniFFI transport.
 * Every invocation creates a fresh typed config and transport.
 */
internal class ConnectionAttemptClientFactory(
    private val clientFactory: social.waddle.android.client.ClientFactory,
    private val sessionPrefs: SessionPrefs,
) {
    suspend fun resource(): String = RESOURCE_PREFIX + sessionPrefs.resourceSuffix()

    fun create(
        session: WaddleSessionInfo,
        resource: String,
        prepared: social.waddle.android.client.DeliveryJournalStore.AttemptBootstrap,
    ): social.waddle.client.ffi.WaddleClientInterface = clientFactory.create(
        social.waddle.client.ffi.WaddleConfig(
            serverUrl = session.xmppWebsocketUrl,
            jid = session.jid,
            accessToken = session.sessionId,
            resource = resource,
            resumeState = prepared.resumeSnapshot?.toFfi(),
            deliveryAttempt = prepared.attempt.toFfi(),
        ),
    )

    private companion object {
        const val RESOURCE_PREFIX = "waddle-android-"
    }
}

/** Owns lifecycle progression and retry policy, never an attempt transport. */
internal class ConnectionLoop(
    attemptClientFactory: ConnectionAttemptClientFactory,
    private val networkSignal: NetworkSignal,
    resume: ResumePersistence,
    router: social.waddle.android.client.XmppEventRouter,
    messenger: social.waddle.android.client.OutboundMessenger,
    private val configuration: ConnectionLoopConfiguration,
) {
    private val attemptRunner = ConnectionAttemptRunner(
        attemptClientFactory = attemptClientFactory,
        resume = resume,
        router = router,
        messenger = messenger,
        connectTimeoutMillis = configuration.connectTimeoutMillis,
    )

    private val _state = MutableStateFlow<ConnectionState>(ConnectionState.Idle)
    val state: StateFlow<ConnectionState> = _state.asStateFlow()

    private val retryRequests = Channel<Unit>(Channel.CONFLATED)

    @Volatile
    private var admissionsOpen = true

    fun resetToIdle() {
        _state.value = ConnectionState.Idle
    }

    fun startAdmissions() {
        admissionsOpen = true
    }

    fun stopAdmissions() {
        admissionsOpen = false
    }

    fun requestReconnect() {
        retryRequests.trySend(Unit)
    }

    suspend fun run(
        session: WaddleSessionInfo,
        lifecycle: SessionLifecycleRef,
    ) {
        var attempt = 0
        while (currentCoroutineContext().isActive && admissionsOpen) {
            waitUntilOnline()
            if (!admissionsOpen) return
            _state.value = ConnectionState.Connecting
            val outcome = try {
                attemptRunner.run(session, lifecycle) { scope, client, readySession, freshStream, readyLifecycle ->
                    _state.value = ConnectionState.Ready
                    configuration.onReady(scope, client, readySession, freshStream, readyLifecycle)
                }
            } catch (cancellation: CancellationException) {
                throw cancellation
            }
            when (outcome) {
                is ConnectionAttemptOutcome.TerminalAuthenticationFailure -> {
                    _state.value = ConnectionState.AuthenticationStopped(
                        condition = outcome.condition,
                        disposition = outcome.disposition,
                    )
                    configuration.onAuthenticationStopped(lifecycle, outcome.disposition)
                    return
                }
                ConnectionAttemptOutcome.FencedOrReplaced -> return
                ConnectionAttemptOutcome.CleanPostReadyDisconnect -> attempt = 0
                ConnectionAttemptOutcome.RetryableFailure -> Unit
            }
            val delayMillis = configuration.reconnectPolicy.delayMillisFor(attempt)
            if (delayMillis == null) {
                _state.value = ConnectionState.Failed
                awaitRecoveryTrigger()
                attempt = 0
                continue
            }
            attempt += 1
            _state.value = ConnectionState.Reconnecting(attempt, delayMillis)
            awaitRetryWindow(delayMillis)
        }
    }

    private suspend fun waitUntilOnline() {
        if (networkSignal.online.first()) return
        _state.value = ConnectionState.Offline
        networkSignal.online.first { it }
    }

    private suspend fun awaitRetryWindow(delayMillis: Long) {
        val wentOffline = withTimeoutOrNull(delayMillis) {
            networkSignal.online.first { online -> !online }
        } != null
        if (wentOffline) {
            _state.value = ConnectionState.Offline
            networkSignal.online.first { it }
        }
    }

    private suspend fun awaitRecoveryTrigger() {
        merge(
            retryRequests.receiveAsFlow(),
            networkSignal.online.drop(1).filter { it }.map { },
        ).first()
    }

    companion object {
        /** Web parity: 15s budget from connect to `SessionReady`. */
        const val CONNECT_TIMEOUT_MILLIS = 15_000L
    }
}
