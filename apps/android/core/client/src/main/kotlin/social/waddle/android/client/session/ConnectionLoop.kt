package social.waddle.android.client.session

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.async
import kotlinx.coroutines.cancelAndJoin
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
import kotlinx.coroutines.selects.select
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import social.waddle.android.client.ClientFactory
import social.waddle.android.client.ConnectionState
import social.waddle.android.client.NetworkSignal
import social.waddle.android.client.OutboundMessenger
import social.waddle.android.client.OutboundQueue
import social.waddle.android.client.ReconnectPolicy
import social.waddle.android.client.SaslRetryDisposition
import social.waddle.android.client.SessionReadyKind
import social.waddle.android.client.XmppEvent
import social.waddle.android.client.XmppEventRouter
import social.waddle.android.client.retryDisposition
import social.waddle.android.client.toXmppEvent
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.toDomain
import social.waddle.android.client.prefs.toFfi
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleConfig
import social.waddle.client.ffi.WaddleSaslCondition

/**
 * The supervised reconnect loop: each attempt builds a fresh
 * `WaddleConfig` (with the persisted XEP-0198 resume snapshot), a fresh
 * bridge, and a fresh client from the injected [ClientFactory], then
 * waits up to the connect budget for `SessionReady`. Failed attempts
 * back off via [ReconnectPolicy]; typed SASL failures decide whether this
 * login/config generation may retry.
 */
internal data class ConnectionLoopCallbacks(
    val onReady: SessionReadyListener,
    val onAuthenticationStopped: suspend (SaslRetryDisposition) -> Unit,
)

internal data class ConnectionLoopSettings(
    val reconnectPolicy: ReconnectPolicy = ReconnectPolicy(),
    val connectTimeoutMillis: Long = ConnectionLoop.CONNECT_TIMEOUT_MILLIS,
)

internal class ConnectionLoop(
    private val clientFactory: ClientFactory,
    private val networkSignal: NetworkSignal,
    private val sessionPrefs: SessionPrefs,
    private val activeSession: ActiveSession,
    private val resume: ResumePersistence,
    private val router: XmppEventRouter,
    private val messenger: OutboundMessenger,
    private val callbacks: ConnectionLoopCallbacks,
    private val settings: ConnectionLoopSettings = ConnectionLoopSettings(),
) {
    private val _state = MutableStateFlow<ConnectionState>(ConnectionState.Idle)
    val state: StateFlow<ConnectionState> = _state.asStateFlow()

    private val retryRequests = Channel<Unit>(Channel.CONFLATED)

    @Volatile
    private var admissionsOpen = true

    /** Logout: the loop is cancelled; publish `Idle` for the UI. */
    fun resetToIdle() {
        _state.value = ConnectionState.Idle
    }

    fun startAdmissions() {
        admissionsOpen = true
    }

    fun stopAdmissions() {
        admissionsOpen = false
    }

    /** Manual retry from the Failed banner: fresh budget immediately. */
    fun requestReconnect() {
        retryRequests.trySend(Unit)
    }

    suspend fun run(session: WaddleSessionInfo) {
        var attempt = 0
        while (currentCoroutineContext().isActive && admissionsOpen) {
            waitUntilOnline()
            if (!admissionsOpen) return
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
                AttemptEnd.ConnectFailed
            }
            when (end) {
                is AttemptEnd.AuthenticationStopped -> {
                    _state.value = ConnectionState.AuthenticationStopped(
                        condition = end.condition,
                        disposition = end.disposition,
                    )
                    callbacks.onAuthenticationStopped(end.disposition)
                    return
                }
                AttemptEnd.DroppedAfterReady -> attempt = 0
                AttemptEnd.ConnectFailed -> Unit
            }
            val delayMillis = settings.reconnectPolicy.delayMillisFor(attempt)
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
        val ownerBareJid = social.waddle.android.client.bareJid(session.jid)
        val resource = RESOURCE_PREFIX + sessionPrefs.resourceSuffix()
        val prepared = messenger.prepareAttempt(ownerBareJid)
        val attempt = activeSession.beginAttempt(prepared.attempt)
        val bridge = attempt.bridge
        var client: WaddleClientInterface? = null
        var activeAttempt: ActiveAttempt? = null
        try {
            val config = buildConfig(session, resource, prepared)
            val liveClient = clientFactory.create(config)
            client = liveClient
            val connected = withTimeoutOrNull(settings.connectTimeoutMillis) {
                try {
                    liveClient.connect()
                    true
                } catch (cancellation: CancellationException) {
                    throw cancellation
                } catch (_: Throwable) {
                    false
                }
            } == true
            if (!connected) return AttemptEnd.ConnectFailed
            return coroutineScope {
                val attemptState = ActiveAttempt(
                    client = liveClient,
                    session = session,
                    scope = this,
                    ownerBareJid = ownerBareJid,
                    deliveryAttempt = prepared.attempt,
                )
                activeAttempt = attemptState
                consumeEvents(
                    nativeClient = liveClient,
                    localEvents = bridge.events,
                    attempt = attemptState,
                )
            }
        } finally {
            val finalAttempt = activeAttempt?.deliveryAttempt
                ?: activeSession.attemptRef
                ?: prepared.attempt
            withContext(NonCancellable) {
                activeSession.endAttempt(finalAttempt)
                messenger.retireAttempt(finalAttempt)
                // Once event consumption began, its ordered teardown already
                // disconnected Rust to wake and join the pending pull.
                if (activeAttempt == null) {
                    runCatching { client?.disconnect() }
                }
            }
            (client as? AutoCloseable)?.close()
        }
    }

    /**
     * Pulls one native event at a time and merges Kotlin-local projections
     * without prefetching across a durability barrier.
     */
    private suspend fun consumeEvents(
        nativeClient: WaddleClientInterface,
        localEvents: ReceiveChannel<XmppEvent>,
        attempt: ActiveAttempt,
    ): AttemptEnd = coroutineScope {
        var nativePoll: Deferred<WaddleClientEvent>? = null

        suspend fun nextOrdered(): OrderedPull {
            val poll = nativePoll ?: async { nativeClient.nextEvent() }.also {
                nativePoll = it
            }
            return select {
                poll.onAwait { event ->
                    nativePoll = null
                    OrderedPull.Native(event)
                }
                localEvents.onReceiveCatching { result ->
                    result.getOrNull()?.let(OrderedPull::Local) ?: OrderedPull.LocalClosed
                }
            }
        }

        suspend fun nextDomain(): DomainPull {
            while (true) {
                when (val pulled = nextOrdered()) {
                    OrderedPull.LocalClosed -> return DomainPull.Fenced
                    is OrderedPull.Local -> return DomainPull.Event(pulled.event)
                    is OrderedPull.Native -> when (
                        val converted = handleNativeControl(pulled.event, attempt)
                    ) {
                        NativeControl.Consumed -> continue
                        NativeControl.Fenced -> return DomainPull.Fenced
                        is NativeControl.Event -> return DomainPull.Event(converted.event)
                    }
                }
            }
        }

        try {
            val readiness = withTimeoutOrNull(settings.connectTimeoutMillis) {
                awaitReadiness(::nextDomain)
            }
            when (readiness) {
                null, Readiness.Closed -> return@coroutineScope AttemptEnd.ConnectFailed
                is Readiness.AuthenticationStopped ->
                    return@coroutineScope AttemptEnd.AuthenticationStopped(
                        readiness.condition,
                        readiness.disposition,
                    )
                is Readiness.Ready -> Unit
            }
            activeSession.onReady(attempt.client, readiness.attempt)
            _state.value = ConnectionState.Ready
            callbacks.onReady(
                attempt.scope,
                attempt.client,
                attempt.session,
                readiness.kind == SessionReadyKind.FRESH,
            )
            // Auth classification is deliberately confined to the pre-ready
            // phase: after the session is bound, "not-authorized"/"forbidden"
            // shaped text also arrives on per-operation stanza errors, and
            // treating those as terminal would sign the user out mid-session.
            while (currentCoroutineContext().isActive) {
                when (val pulled = nextDomain()) {
                    DomainPull.Fenced -> return@coroutineScope AttemptEnd.DroppedAfterReady
                    is DomainPull.Event -> {
                        val event = pulled.event
                        if (!messenger.reconcileDeliveryEvent(event)) continue
                        router.dispatch(event)
                        if (event is XmppEvent.Disconnected) {
                            return@coroutineScope AttemptEnd.DroppedAfterReady
                        }
                    }
                }
            }
            AttemptEnd.DroppedAfterReady
        } finally {
            // A pending UniFFI future is a structured child of this scope.
            // Fence sends first, then make Rust publish a new lifecycle epoch
            // so `next_event` wakes, and only then cancel/join the child.
            withContext(NonCancellable) {
                activeSession.endAttempt(attempt.deliveryAttempt)
                runCatching { nativeClient.disconnect() }
                nativePoll?.cancelAndJoin()
            }
        }
    }

    private suspend fun awaitReadiness(nextDomain: suspend () -> DomainPull): Readiness {
        while (true) {
            val event = when (val pulled = nextDomain()) {
                DomainPull.Fenced -> return Readiness.Closed
                is DomainPull.Event -> pulled.event
            }
            if (!messenger.reconcileDeliveryEvent(event)) continue
            router.dispatch(event)
            when (event) {
                is XmppEvent.SessionReady -> return Readiness.Ready(event.kind, event.attempt)
                // Typed SASL failure from the FFI is the only authentication
                // classification signal. Only RFC 6120
                // `temporary-auth-failure` may reuse this generation.
                is XmppEvent.AuthenticationFailed -> {
                    val disposition = event.condition.retryDisposition()
                    return if (disposition == SaslRetryDisposition.RETRY) {
                        Readiness.Closed
                    } else {
                        Readiness.AuthenticationStopped(event.condition, disposition)
                    }
                }
                is XmppEvent.Disconnected -> return Readiness.Closed
                else -> Unit
            }
        }
    }

    private suspend fun handleNativeControl(
        event: WaddleClientEvent,
        activeAttempt: ActiveAttempt,
    ): NativeControl {
        val ownerBareJid = activeAttempt.ownerBareJid
        return when (event) {
            is WaddleClientEvent.ResumeFailed -> {
                val affected = event.affected.map { it.value }
                if (affected.size != affected.toSet().size) {
                    NativeControl.Fenced
                } else {
                    val transition = try {
                        event.transition.toDomain(ownerBareJid)
                    } catch (_: IllegalArgumentException) {
                        return NativeControl.Fenced
                    }
                    if (
                        transition.old != activeSession.attemptRef ||
                        !messenger.rotateAndAwait(transition, affected.toSet())
                    ) {
                        NativeControl.Fenced
                    } else {
                        activeAttempt.deliveryAttempt = transition.fresh
                        NativeControl.Consumed
                    }
                }
            }
            is WaddleClientEvent.ResumeStateChanged -> {
                val attempt = try {
                    event.attempt.toDomain(ownerBareJid)
                } catch (_: IllegalArgumentException) {
                    return NativeControl.Fenced
                }
                if (
                    attempt != activeSession.attemptRef ||
                    !resume.persistResumeSnapshot(attempt, event.state)
                ) {
                    NativeControl.Fenced
                } else {
                    NativeControl.Consumed
                }
            }
            else -> {
                val domain = try {
                    event.toXmppEvent(ownerBareJid)
                } catch (_: IllegalArgumentException) {
                    return NativeControl.Fenced
                } ?: return NativeControl.Fenced
                val eventAttempt = when (domain) {
                    is XmppEvent.SessionReady -> domain.attempt
                    is XmppEvent.NativeDeliveryAcked -> domain.attempt
                    is XmppEvent.NativeDeliveryFailed -> domain.attempt
                    else -> null
                }
                if (eventAttempt != null && eventAttempt != activeSession.attemptRef) {
                    NativeControl.Fenced
                } else {
                    NativeControl.Event(domain)
                }
            }
        }
    }

    private fun buildConfig(
        session: WaddleSessionInfo,
        resource: String,
        prepared: OutboundQueue.AttemptBootstrap,
    ): WaddleConfig = WaddleConfig(
        serverUrl = session.xmppWebsocketUrl,
        jid = session.jid,
        accessToken = session.sessionId,
        resource = resource,
        resumeState = prepared.resumeSnapshot?.toFfi(),
        deliveryAttempt = prepared.attempt.toFfi(),
    )

    private data class ActiveAttempt(
        val client: WaddleClientInterface,
        val session: WaddleSessionInfo,
        val scope: CoroutineScope,
        val ownerBareJid: String,
        var deliveryAttempt: social.waddle.android.client.prefs.DeliveryAttemptRef,
    )

    private sealed interface OrderedPull {
        data class Native(val event: WaddleClientEvent) : OrderedPull
        data class Local(val event: XmppEvent) : OrderedPull
        data object LocalClosed : OrderedPull
    }

    private sealed interface DomainPull {
        data class Event(val event: XmppEvent) : DomainPull
        data object Fenced : DomainPull
    }

    private sealed interface NativeControl {
        data class Event(val event: XmppEvent) : NativeControl
        data object Consumed : NativeControl
        data object Fenced : NativeControl
    }

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

    private sealed interface AttemptEnd {
        data class AuthenticationStopped(
            val condition: WaddleSaslCondition,
            val disposition: SaslRetryDisposition,
        ) : AttemptEnd

        data object ConnectFailed : AttemptEnd
        data object DroppedAfterReady : AttemptEnd
    }

    private sealed interface Readiness {
        data class Ready(
            val kind: SessionReadyKind,
            val attempt: social.waddle.android.client.prefs.DeliveryAttemptRef,
        ) : Readiness

        data class AuthenticationStopped(
            val condition: WaddleSaslCondition,
            val disposition: SaslRetryDisposition,
        ) : Readiness

        data object Closed : Readiness
    }

    companion object {
        /** Web parity: 15s budget from connect to `SessionReady`. */
        const val CONNECT_TIMEOUT_MILLIS = 15_000L

        private const val RESOURCE_PREFIX = "waddle-android-"
    }
}

/**
 * Callback fired on the attempt's scope once `SessionReady` lands:
 * receives the live client and whether the stream is fresh (needs
 * catch-up) so the owner can launch the ready pipeline.
 */
internal typealias SessionReadyListener = (
    attemptScope: CoroutineScope,
    client: WaddleClientInterface,
    session: WaddleSessionInfo,
    freshStream: Boolean,
) -> Unit
