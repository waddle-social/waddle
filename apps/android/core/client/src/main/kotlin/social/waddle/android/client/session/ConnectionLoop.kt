package social.waddle.android.client.session

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.async
import kotlinx.coroutines.cancelChildren
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.channels.ReceiveChannel
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.merge
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.job
import kotlinx.coroutines.launch
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
 *
 * A ready stream is never trusted on faith. Nothing in the WebSocket
 * transport notices a half-open socket (Doze, NAT timeout, a network
 * handoff), so while `Ready` the loop probes the server with an XEP-0199
 * ping whenever the app comes to the foreground, the default network
 * changes, an ordinary send fails at the transport, and periodically.
 * A failed or late probe retires the attempt and reconnects at once;
 * losing the network retires it without a probe.
 *
 * The loop only connects while [wanted] says something needs the stream.
 * When demand drops, the live attempt ends with a clean close (no
 * XEP-0198 resumption is ever requested, so the server ends the session
 * at once and pushes flow) and the loop parks in `Paused` until demand
 * returns.
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
    /**
     * Periodic probe cadence while ready, injected by the app (see
     * [fixedRateTicks]). Empty by default: a perpetual timer would keep
     * virtual-time test schedulers from ever going idle.
     */
    private val livenessTicks: Flow<Unit> = emptyFlow(),
    private val pingTimeoutMillis: Long = PING_TIMEOUT_MILLIS,
    /** Whether anything needs the stream now (the app derives it). */
    private val wanted: StateFlow<Boolean> = MutableStateFlow(true),
) {
    private val _state = MutableStateFlow<ConnectionState>(ConnectionState.Idle)
    val state: StateFlow<ConnectionState> = _state.asStateFlow()

    private val retryRequests = Channel<Unit>(Channel.CONFLATED)
    private val livenessRequests = Channel<Unit>(Channel.CONFLATED)

    /** Logout: the loop is cancelled; publish `Idle` for the UI. */
    fun resetToIdle() {
        _state.value = ConnectionState.Idle
    }

    /** Skip the remaining backoff and attempt now (manual retry, foreground). */
    fun requestReconnect() {
        retryRequests.trySend(Unit)
    }

    /**
     * Ask the ready stream to prove it is alive. Called on app foreground
     * and after a transport-level send failure; a no-op unless `Ready`.
     */
    fun requestLivenessCheck() {
        livenessRequests.trySend(Unit)
    }

    suspend fun run(session: WaddleSessionInfo, sessionScope: CoroutineScope) {
        var attempt = 0
        while (currentCoroutineContext().isActive) {
            awaitConnectable()
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
                is AttemptEnd.AuthFailed -> {
                    _state.value = ConnectionState.AuthFailed
                    callbacks.onTerminalAuthFailure(end.lease, sessionScope)
                    return
                }
                AttemptEnd.DROPPED_AFTER_READY -> attempt = 0
                // A ready stream that failed its liveness probe was dead
                // for an unknown time already: reconnect without waiting.
                // A released stream ended on purpose: no backoff either,
                // the loop parks until demand returns.
                AttemptEnd.STALE_AFTER_READY, AttemptEnd.RELEASED -> {
                    attempt = 0
                    continue
                }
                AttemptEnd.CONNECT_FAILED -> Unit
            }
            val delayMillis = reconnectPolicy.delayMillisFor(attempt)
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
        val attempt = activeSession.beginAttempt() ?: return AttemptEnd.CONNECT_FAILED
        val bridge = attempt.bridge
        val config = buildConfig(session)
        // The full JID the call engine signs Jingle stanzas with
        // (initiator/responder attributes, tie-break comparand).
        val ownFullJid = "${config.jid}/${config.resource}"
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
                    consumeEvents(
                        AttemptEventConsumer(
                            events = bridge.events,
                            client = client,
                            session = session,
                            attempt = attempt,
                            ownFullJid = ownFullJid,
                            scope = this,
                        ),
                    )
                }
                val released = async {
                    wanted.first { !it }
                    AttemptEnd.RELEASED
                }
                val end = select<AttemptEnd?> {
                    consumer.onAwait { it }
                    released.onAwait { it }
                    connector.onAwait { failure ->
                        if (failure == null) null else AttemptEnd.CONNECT_FAILED
                    }
                } ?: select {
                    consumer.onAwait { it }
                    released.onAwait { it }
                }
                coroutineContext.job.cancelChildren()
                end
            }
        } finally {
            activeSession.endAttempt(attempt, client)
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
    private data class AttemptEventConsumer(
        val events: ReceiveChannel<XmppEvent>,
        val client: WaddleClientInterface,
        val session: WaddleSessionInfo,
        val attempt: ActiveSession.Attempt,
        val ownFullJid: String,
        val scope: CoroutineScope,
    )

    private suspend fun consumeEvents(consumer: AttemptEventConsumer): AttemptEnd {
        val readiness = withTimeoutOrNull(connectTimeoutMillis) { awaitReadiness(consumer.events) }
        when (readiness) {
            null, Readiness.CLOSED -> return AttemptEnd.CONNECT_FAILED
            Readiness.AUTH_FAILED -> return AttemptEnd.AuthFailed(consumer.attempt.lease)
            Readiness.READY -> Unit
        }
        val published = activeSession.publishReady(
            attempt = consumer.attempt,
            readyClient = consumer.client,
            readyOwnFullJid = consumer.ownFullJid,
            onPublished = {
                callbacks.onReady(consumer.scope, consumer.session, true, consumer.attempt.lease)
            },
        )
        if (!published) return AttemptEnd.CONNECT_FAILED
        _state.value = ConnectionState.Ready
        // Native FFI clients deliberately start fresh streams: the web-only
        // typed SM persistence path owns resumable browser sessions, while
        // Android catch-up is durable and idempotent.
        // Auth classification is deliberately confined to the pre-ready
        // phase: after the session is bound, "not-authorized"/"forbidden"
        // shaped text also arrives on per-operation stanza errors, and
        // treating those as terminal would sign the user out mid-session.
        // A request queued while the previous attempt was not ready is
        // moot: this stream is brand new.
        livenessRequests.tryReceive()
        val retired = CompletableDeferred<AttemptEnd>()
        val watchdog = consumer.scope.launch { superviseLiveness(consumer.client, retired) }
        try {
            while (true) {
                val end = select<AttemptEnd?> {
                    retired.onAwait { it }
                    consumer.events.onReceiveCatching { result ->
                        val event = result.getOrNull() ?: return@onReceiveCatching AttemptEnd.DROPPED_AFTER_READY
                        dispatch(event)
                        if (event is XmppEvent.Disconnected) AttemptEnd.DROPPED_AFTER_READY else null
                    }
                }
                if (end != null) return end
            }
        } finally {
            watchdog.cancel()
        }
    }

    /**
     * Completes [retired] once the ready stream can no longer be trusted:
     * the network disappeared ([AttemptEnd.DROPPED_AFTER_READY], the
     * outer loop parks in `Offline`) or a liveness probe failed
     * ([AttemptEnd.STALE_AFTER_READY], reconnect immediately).
     */
    private suspend fun superviseLiveness(
        client: WaddleClientInterface,
        retired: CompletableDeferred<AttemptEnd>,
    ) = coroutineScope {
        launch {
            networkSignal.online.first { online -> !online }
            retired.complete(AttemptEnd.DROPPED_AFTER_READY)
        }
        val triggers = merge(
            livenessRequests.receiveAsFlow(),
            networkSignal.networkChanges,
            livenessTicks,
        )
        triggers.first { !isAlive(client) }
        retired.complete(AttemptEnd.STALE_AFTER_READY)
    }

    /** XEP-0199 round trip within [pingTimeoutMillis]; any failure means dead. */
    private suspend fun isAlive(client: WaddleClientInterface): Boolean = try {
        withTimeoutOrNull(pingTimeoutMillis) { client.pingServer() } != null
    } catch (cancellation: CancellationException) {
        throw cancellation
    } catch (_: Throwable) {
        false
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
        if (event is XmppEvent.MessageRejected && !callbacks.onMessageRejected(event)) return
        router.dispatch(event)
    }

    private suspend fun buildConfig(session: WaddleSessionInfo): WaddleConfig = WaddleConfig(
        serverUrl = session.xmppWebsocketUrl,
        jid = session.jid,
        accessToken = session.sessionId,
        resource = RESOURCE_PREFIX + sessionPrefs.resourceSuffix(),
        // Android never resumes: a session the server parks for a
        // resumption that never comes suppresses DM pushes until it
        // expires.
        resumable = false,
    )

    /**
     * Park until a connect makes sense: `Paused` while nothing wants the
     * stream, `Offline` while there is no network. Either can change
     * while the other is awaited, so both are rechecked together.
     */
    private suspend fun awaitConnectable() {
        while (true) {
            when {
                !wanted.value -> {
                    _state.value = ConnectionState.Paused
                    wanted.first { it }
                }
                !networkSignal.online.first() -> {
                    _state.value = ConnectionState.Offline
                    merge(
                        networkSignal.online.filter { it },
                        wanted.filter { !it },
                    ).first()
                }
                else -> return
            }
        }
    }

    /**
     * Wait out the backoff delay unless something better happens first:
     * an explicit retry request (user tap, app foreground) or a network
     * handoff skips the rest of the wait; losing the network or the
     * demand ends it too, and [awaitConnectable] parks in `Offline` or
     * `Paused` until that comes back.
     */
    private suspend fun awaitRetryWindow(delayMillis: Long) {
        withTimeoutOrNull(delayMillis) {
            merge(
                retryRequests.receiveAsFlow(),
                networkSignal.networkChanges,
                networkSignal.online.filter { online -> !online }.map { },
                wanted.filter { !it }.map { },
            ).first()
        }
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

    /** The terminal result retains the exact pre-ready attempt authority. */
    private sealed interface AttemptEnd {
        data class AuthFailed(val lease: ActiveSession.OwnerLease) : AttemptEnd
        data object CONNECT_FAILED : AttemptEnd
        data object DROPPED_AFTER_READY : AttemptEnd

        /** The ready stream failed a liveness probe (half-open socket). */
        data object STALE_AFTER_READY : AttemptEnd

        /** Nothing wanted the stream any more; it was closed on purpose. */
        data object RELEASED : AttemptEnd
    }

    private enum class Readiness { READY, AUTH_FAILED, CLOSED }

    companion object {
        /** Web parity: 15s budget from connect to `SessionReady`. */
        const val CONNECT_TIMEOUT_MILLIS = 15_000L

        /**
         * Production probe cadence while ready. Foreground and network
         * changes probe immediately; this only bounds how long a silently
         * dead stream can go unnoticed when nothing else happens.
         */
        const val LIVENESS_INTERVAL_MILLIS = 180_000L

        /** The Rust ping gives up after 8s; allow a little scheduling slack. */
        const val PING_TIMEOUT_MILLIS = 10_000L

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
    lease: ActiveSession.OwnerLease,
) -> Unit

/** Typed lifecycle callbacks kept together as one constructor dependency. */
internal class ConnectionLoopCallbacks(
    val onReady: SessionReadyListener,
    val onDeliveryAcked: suspend (String) -> Unit,
    val onMessageRejected: suspend (XmppEvent.MessageRejected) -> Boolean,
    /**
     * Carries both the failing attempt's immutable lease and the exact
     * manager scope that launched this loop. The receiver must validate both
     * before touching lifecycle state: a cancelled old loop can report after
     * a same-account or different-account successor has been installed.
     */
    val onTerminalAuthFailure: (ActiveSession.OwnerLease, CoroutineScope) -> Unit,
)

/** A tick every [intervalMillis], forever, for [ConnectionLoop]'s liveness cadence. */
fun fixedRateTicks(intervalMillis: Long = ConnectionLoop.LIVENESS_INTERVAL_MILLIS): Flow<Unit> = flow {
    while (true) {
        delay(intervalMillis)
        emit(Unit)
    }
}
