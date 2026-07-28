package social.waddle.android.client.session

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.async
import kotlinx.coroutines.cancelChildren
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.channels.ReceiveChannel
import kotlinx.coroutines.coroutineScope
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
import kotlinx.coroutines.job
import kotlinx.coroutines.selects.select
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import social.waddle.android.client.ClientFactory
import social.waddle.android.client.ConnectionState
import social.waddle.android.client.NetworkSignal
import social.waddle.android.client.ReconnectPolicy
import social.waddle.android.client.XmppEvent
import social.waddle.android.client.XmppEventRouter
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleConfig
import social.waddle.client.ffi.WaddleSaslCondition

/**
 * The supervised reconnect loop: each attempt builds a fresh `WaddleConfig`,
 * bridge, and client from the injected [ClientFactory], then
 * waits up to the connect budget for `SessionReady`. Failed attempts
 * back off via [ReconnectPolicy]; credential-shaped SASL failures are
 * terminal and reported via [onTerminalAuthFailure].
 */
internal class ConnectionLoop(
    private val clientFactory: ClientFactory,
    private val networkSignal: NetworkSignal,
    private val sessionPrefs: SessionPrefs,
    private val activeSession: ActiveSession,
    private val router: XmppEventRouter,
    private val callbacks: ConnectionLoopCallbacks,
    private val reconnectPolicy: ReconnectPolicy = ReconnectPolicy(),
    private val connectTimeoutMillis: Long = CONNECT_TIMEOUT_MILLIS,
) {
    private val _state = MutableStateFlow<ConnectionState>(ConnectionState.Idle)
    val state: StateFlow<ConnectionState> = _state.asStateFlow()

    private val retryRequests = Channel<Unit>(Channel.CONFLATED)

    /** Logout: the loop is cancelled; publish `Idle` for the UI. */
    fun resetToIdle() {
        _state.value = ConnectionState.Idle
    }

    /** Manual retry from the Failed banner: fresh budget immediately. */
    fun requestReconnect() {
        retryRequests.trySend(Unit)
    }

    suspend fun run(session: WaddleSessionInfo) {
        var attempt = 0
        while (currentCoroutineContext().isActive) {
            waitUntilOnline()
            _state.value = ConnectionState.Connecting
            // The attempt touches DataStore (buildConfig reads the resume
            // snapshot/resource suffix): IOException on a corrupt or full
            // store must back off like any failed attempt — escaping this
            // handler-less root coroutine would crash-loop the process.
            val end = try {
                runAttempt(session)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (_: Throwable) {
                AttemptEnd.CONNECT_FAILED
            }
            when (end) {
                AttemptEnd.AUTH_FAILED -> {
                    _state.value = ConnectionState.AuthFailed
                    callbacks.onTerminalAuthFailure()
                    return
                }
                AttemptEnd.DROPPED_AFTER_READY -> attempt = 0
                AttemptEnd.CONNECT_FAILED -> Unit
            }
            val delayMillis = reconnectPolicy.delayMillisFor(attempt)
            if (delayMillis == null) {
                // Budget spent: park instead of abandoning the session.
                // Web parity (armOnlineRecovery/connectWithFreshBudget):
                // a genuine offline->online transition or an explicit user
                // retry restarts the loop with a fresh attempt budget.
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

    /**
     * One connection attempt: fresh config + bridge + client. `connect()`
     * races the event consumer so a thrown connect failure aborts the
     * attempt immediately instead of waiting out the connect budget.
     */
    private suspend fun runAttempt(session: WaddleSessionInfo): AttemptEnd {
        val bridge = activeSession.beginAttempt()
        val config = buildConfig(session)
        // The full JID the call engine signs Jingle stanzas with
        // (initiator/responder attributes, tie-break comparand).
        activeSession.ownFullJid = "${config.jid}/${config.resource}"
        val client = clientFactory.create(config, bridge)
        try {
            return coroutineScope {
                val connector = async {
                    try {
                        client.connect()
                        null
                    } catch (cancellation: CancellationException) {
                        throw cancellation
                    } catch (failure: Throwable) {
                        failure
                    }
                }
                val consumer = async {
                    consumeEvents(bridge.events, client, session, this)
                }
                val end = select<AttemptEnd?> {
                    consumer.onAwait { it }
                    connector.onAwait { failure ->
                        if (failure == null) null else AttemptEnd.CONNECT_FAILED
                    }
                } ?: consumer.await()
                coroutineContext.job.cancelChildren()
                end
            }
        } finally {
            activeSession.endAttempt(client)
            withContext(NonCancellable) {
                runCatching { client.disconnect() }
            }
            (client as? AutoCloseable)?.close()
        }
    }

    /**
     * Drains the bridge channel for the lifetime of the attempt. Phase 1
     * waits for `SessionReady` under the connect budget; phase 2 keeps
     * fanning events out until the stream ends.
     */
    private suspend fun consumeEvents(
        events: ReceiveChannel<XmppEvent>,
        client: WaddleClientInterface,
        session: WaddleSessionInfo,
        attemptScope: CoroutineScope,
    ): AttemptEnd {
        val readiness = withTimeoutOrNull(connectTimeoutMillis) { awaitReadiness(events) }
        when (readiness) {
            null, Readiness.CLOSED -> return AttemptEnd.CONNECT_FAILED
            Readiness.AUTH_FAILED -> return AttemptEnd.AUTH_FAILED
            Readiness.READY -> Unit
        }
        activeSession.onReady(client)
        _state.value = ConnectionState.Ready
        // Native FFI clients deliberately start fresh streams: the web-only
        // typed SM persistence path owns resumable browser sessions, while
        // Android catch-up is durable and idempotent.
        callbacks.onReady(attemptScope, session, true)
        // Auth classification is deliberately confined to the pre-ready
        // phase: after the session is bound, "not-authorized"/"forbidden"
        // shaped text also arrives on per-operation stanza errors, and
        // treating those as terminal would sign the user out mid-session.
        for (event in events) {
            dispatch(event)
            if (event is XmppEvent.Disconnected) return AttemptEnd.DROPPED_AFTER_READY
        }
        return AttemptEnd.DROPPED_AFTER_READY
    }

    private suspend fun awaitReadiness(events: ReceiveChannel<XmppEvent>): Readiness {
        for (event in events) {
            dispatch(event)
            when (event) {
                is XmppEvent.SessionReady -> return Readiness.READY
                // Typed SASL failure from the FFI: the ONLY terminal auth
                // signal, and only for credential-shaped conditions — RFC
                // 6120 §6.5 temporary-auth-failure (and mechanism/encoding
                // conditions) must retry, not wipe the session (web #1164).
                is XmppEvent.AuthenticationFailed ->
                    return if (isTerminalSaslCondition(event.condition)) {
                        Readiness.AUTH_FAILED
                    } else {
                        Readiness.CLOSED
                    }
                is XmppEvent.Disconnected -> return Readiness.CLOSED
                else -> Unit
            }
        }
        return Readiness.CLOSED
    }

    /** Durable queue acknowledgement must complete before UI fan-out. */
    private suspend fun dispatch(event: XmppEvent) {
        if (event is XmppEvent.DeliveryAcked) {
            callbacks.onDeliveryAcked(event.stanzaId)
        }
        router.dispatch(event)
    }

    private suspend fun buildConfig(session: WaddleSessionInfo): WaddleConfig = WaddleConfig(
        serverUrl = session.xmppWebsocketUrl,
        jid = session.jid,
        accessToken = session.sessionId,
        resource = RESOURCE_PREFIX + sessionPrefs.resourceSuffix(),
    )

    /** Park in `Offline` until connectivity exists. */
    private suspend fun waitUntilOnline() {
        if (networkSignal.online.first()) return
        _state.value = ConnectionState.Offline
        networkSignal.online.first { it }
    }

    /**
     * Wait out the backoff delay — unless the network drops, in which
     * case park in `Offline` and retry immediately once it returns
     * (bypassing the remaining timer, web `navigator.onLine` parity).
     */
    private suspend fun awaitRetryWindow(delayMillis: Long) {
        val wentOffline = withTimeoutOrNull(delayMillis) {
            networkSignal.online.first { online -> !online }
        } != null
        if (wentOffline) {
            _state.value = ConnectionState.Offline
            networkSignal.online.first { it }
        }
    }

    /**
     * Parks the exhausted loop until either a real offline->online edge
     * (`drop(1)` skips the replayed current value of the StateFlow-shaped
     * signal) or an explicit retry request. Covers both failure shapes:
     * connectivity loss recovers on the edge, server-side outages recover
     * via user retry (the device never went offline).
     */
    private suspend fun awaitRecoveryTrigger() {
        merge(
            retryRequests.receiveAsFlow(),
            networkSignal.online.drop(1).filter { it }.map { },
        ).first()
    }

    /**
     * Credential-shaped conditions invalidate the stored token; every
     * other condition (temporary-auth-failure, mechanism/encoding
     * mismatches, Unknown) is treated as a failed attempt and retried —
     * the backoff budget parks the loop if it persists.
     */
    private fun isTerminalSaslCondition(condition: WaddleSaslCondition): Boolean =
        when (condition) {
            WaddleSaslCondition.NOT_AUTHORIZED,
            WaddleSaslCondition.ACCOUNT_DISABLED,
            WaddleSaslCondition.CREDENTIALS_EXPIRED,
            -> true
            else -> false
        }

    private enum class AttemptEnd { AUTH_FAILED, CONNECT_FAILED, DROPPED_AFTER_READY }

    private enum class Readiness { READY, AUTH_FAILED, CLOSED }

    companion object {
        /** Web parity: 15s budget from connect to `SessionReady`. */
        const val CONNECT_TIMEOUT_MILLIS = 15_000L

        private const val RESOURCE_PREFIX = "waddle-android-"
    }
}

/**
 * Callback fired on the attempt's scope once `SessionReady` lands:
 * receives whether the stream is fresh (needs catch-up) so the owner can
 * launch the ready pipeline through [ActiveSession]'s transport gateway.
 */
internal typealias SessionReadyListener = (
    attemptScope: CoroutineScope,
    session: WaddleSessionInfo,
    freshStream: Boolean,
) -> Unit

/** Typed lifecycle callbacks kept together as one constructor dependency. */
internal class ConnectionLoopCallbacks(
    val onReady: SessionReadyListener,
    val onDeliveryAcked: suspend (String) -> Unit,
    val onTerminalAuthFailure: suspend () -> Unit,
)
