package social.waddle.android.client.session

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.async
import kotlinx.coroutines.channels.ReceiveChannel
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.isActive
import kotlinx.coroutines.selects.select
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import social.waddle.android.client.AttemptActivation
import social.waddle.android.client.ConnectionAttemptHandle
import social.waddle.android.client.OutboundMessenger
import social.waddle.android.client.ResumeHandoffOutcome
import social.waddle.android.client.SaslRetryDisposition
import social.waddle.android.client.SessionLifecycleRef
import social.waddle.android.client.SessionReadyKind
import social.waddle.android.client.TransportAttachOutcome
import social.waddle.android.client.TransportConstructionClaim
import social.waddle.android.client.XmppEvent
import social.waddle.android.client.XmppEventRouter
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.toDomain
import social.waddle.android.client.retryDisposition
import social.waddle.android.client.toXmppEvent
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleSaslCondition

/** Executes exactly one physical XMPP connection attempt. */
internal class ConnectionAttemptRunner(
    private val attemptClientFactory: ConnectionAttemptClientFactory,
    private val resume: ResumePersistence,
    private val router: XmppEventRouter,
    private val messenger: OutboundMessenger,
    private val connectTimeoutMillis: Long,
) {
    suspend fun run(
        session: WaddleSessionInfo,
        lifecycle: SessionLifecycleRef,
        onReady: SessionReadyListener,
    ): ConnectionAttemptOutcome = try {
        runPhysicalAttempt(session, lifecycle, onReady)
    } catch (cancellation: CancellationException) {
        throw cancellation
    } catch (_: Throwable) {
        ConnectionAttemptOutcome.RetryableFailure
    }

    private suspend fun runPhysicalAttempt(
        session: WaddleSessionInfo,
        lifecycle: SessionLifecycleRef,
        onReady: SessionReadyListener,
    ): ConnectionAttemptOutcome {
        val state = PhysicalAttemptState()
        try {
            val client = constructTransport(session, lifecycle, state)
                ?: return ConnectionAttemptOutcome.FencedOrReplaced
            if (!connect(client)) return ConnectionAttemptOutcome.RetryableFailure
            return consumeConnectedAttempt(client, session, lifecycle, onReady, state)
        } finally {
            cleanupPhysicalAttempt(state)
        }
    }

    private suspend fun constructTransport(
        session: WaddleSessionInfo,
        lifecycle: SessionLifecycleRef,
        state: PhysicalAttemptState,
    ): WaddleClientInterface? {
        val resource = attemptClientFactory.resource()
        val activation = messenger.activateAttempt(lifecycle)
        state.activation = activation
        val construction = messenger.beginTransportConstruction(activation.handle) ?: return null
        state.construction = construction
        val client = attemptClientFactory.create(session, resource, activation.bootstrap)
        state.client = client
        return when (messenger.attachConstructedTransport(construction, client)) {
            TransportAttachOutcome.Attached -> client.also {
                state.construction = null
                state.attachedConstruction = true
            }
            TransportAttachOutcome.SupersededAndClose -> {
                closeAttemptTransport(client)
                messenger.finishSupersededConstruction(construction)
                state.construction = null
                state.client = null
                null
            }
        }
    }

    private suspend fun connect(client: WaddleClientInterface): Boolean =
        withTimeoutOrNull(connectTimeoutMillis) {
            try {
                client.connect()
                true
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (_: Throwable) {
                false
            }
        } == true

    private suspend fun consumeConnectedAttempt(
        client: WaddleClientInterface,
        session: WaddleSessionInfo,
        lifecycle: SessionLifecycleRef,
        onReady: SessionReadyListener,
        state: PhysicalAttemptState,
    ): ConnectionAttemptOutcome = coroutineScope {
        val activation = requireNotNull(state.activation)
        val attempt = ActiveAttempt(
            client = client,
            session = session,
            scope = this,
            lifecycle = lifecycle,
            handle = activation.handle,
        )
        state.activeAttempt = attempt
        consumeEvents(client, activation.bridge.events, attempt, onReady)
    }

    private suspend fun cleanupPhysicalAttempt(state: PhysicalAttemptState) {
        withContext(NonCancellable) {
            state.construction?.let { construction ->
                val constructedClient = state.client
                if (constructedClient != null) {
                    closeAttemptTransport(constructedClient)
                }
                messenger.finishSupersededConstruction(construction)
                state.client = null
            }
            state.activation?.let { activation ->
                val disconnected = state.activeAttempt?.producerQuiesced
                    ?: messenger.disconnectTransport(activation.handle)
                val closed = closeAttemptTransport(state.client)
                if (state.attachedConstruction) {
                    messenger.markTransportClosed(activation.handle, closed)
                }
                messenger.closeAttempt(
                    activation.handle,
                    producerQuiesced = disconnected && closed,
                )
            }
        }
    }

    private suspend fun consumeEvents(
        nativeClient: WaddleClientInterface,
        localEvents: ReceiveChannel<XmppEvent>,
        attempt: ActiveAttempt,
        onReady: SessionReadyListener,
    ): ConnectionAttemptOutcome = coroutineScope {
        val puller = OrderedEventPuller(
            scope = this,
            nativeClient = nativeClient,
            localEvents = localEvents,
            handleNative = { event -> handleNativeControl(event, attempt) },
        )
        try {
            when (
                val readiness = withTimeoutOrNull(connectTimeoutMillis) {
                awaitReadiness(puller::nextDomain)
            }
            ) {
                null -> return@coroutineScope ConnectionAttemptOutcome.RetryableFailure
                Readiness.Fenced -> return@coroutineScope ConnectionAttemptOutcome.FencedOrReplaced
                Readiness.Disconnected -> return@coroutineScope ConnectionAttemptOutcome.RetryableFailure
                is Readiness.AuthenticationStopped ->
                    return@coroutineScope ConnectionAttemptOutcome.TerminalAuthenticationFailure(
                        readiness.condition,
                        readiness.disposition,
                    )
                is Readiness.Ready -> {
                    if (!messenger.markReady(attempt.handle, attempt.client, readiness.attempt)) {
                        return@coroutineScope ConnectionAttemptOutcome.FencedOrReplaced
                    }
                    onReady(
                        attempt.scope,
                        attempt.client,
                        attempt.session,
                        readiness.kind == SessionReadyKind.FRESH,
                        attempt.lifecycle,
                    )
                }
            }
            consumeReadyEvents(puller::nextDomain)
        } finally {
            withContext(NonCancellable) {
                val disconnected = messenger.disconnectTransport(attempt.handle)
                val pullStopped = puller.cancelNativePoll()
                attempt.producerQuiesced = disconnected && pullStopped
            }
        }
    }

    private suspend fun consumeReadyEvents(
        nextDomain: suspend () -> DomainPull,
    ): ConnectionAttemptOutcome {
        while (currentCoroutineContext().isActive) {
            when (val pulled = nextDomain()) {
                DomainPull.Fenced -> return ConnectionAttemptOutcome.FencedOrReplaced
                is DomainPull.Event -> {
                    val event = pulled.event
                    if (!messenger.reconcileDeliveryEvent(event)) continue
                    router.dispatch(event)
                    if (event is XmppEvent.Disconnected) {
                        return ConnectionAttemptOutcome.CleanPostReadyDisconnect
                    }
                }
            }
        }
        return ConnectionAttemptOutcome.FencedOrReplaced
    }

    private suspend fun awaitReadiness(nextDomain: suspend () -> DomainPull): Readiness {
        while (true) {
            val event = when (val pulled = nextDomain()) {
                DomainPull.Fenced -> return Readiness.Fenced
                is DomainPull.Event -> pulled.event
            }
            if (!messenger.reconcileDeliveryEvent(event)) continue
            router.dispatch(event)
            when (event) {
                is XmppEvent.SessionReady -> return Readiness.Ready(event.kind, event.attempt)
                is XmppEvent.AuthenticationFailed -> {
                    val disposition = event.condition.retryDisposition()
                    return if (disposition == SaslRetryDisposition.RETRY) {
                        Readiness.Disconnected
                    } else {
                        Readiness.AuthenticationStopped(event.condition, disposition)
                    }
                }
                is XmppEvent.Disconnected -> return Readiness.Disconnected
                else -> Unit
            }
        }
    }

    private suspend fun handleNativeControl(
        event: WaddleClientEvent,
        attempt: ActiveAttempt,
    ): NativeControl = when (event) {
        is WaddleClientEvent.ResumeFailed -> handleResumeFailure(event, attempt)
        is WaddleClientEvent.ResumeStateChanged -> handleResumeStateChange(event, attempt)
        else -> handleDomainEvent(event, attempt)
    }

    private suspend fun handleResumeFailure(
        event: WaddleClientEvent.ResumeFailed,
        attempt: ActiveAttempt,
    ): NativeControl {
        val affected = event.affected.map { it.value }
        val affectedSet = affected.toSet()
        if (affected.size != affectedSet.size) return NativeControl.Fenced
        val transition = try {
            event.transition.toDomain(attempt.lifecycle.ownerBareJid)
        } catch (_: IllegalArgumentException) {
            return NativeControl.Fenced
        }
        if (
            !messenger.matches(attempt.handle, transition.old) ||
            messenger.rotateAndAwait(attempt.handle, transition, affectedSet) !=
            ResumeHandoffOutcome.Committed
        ) {
            return NativeControl.Fenced
        }
        return NativeControl.Consumed
    }

    private suspend fun handleResumeStateChange(
        event: WaddleClientEvent.ResumeStateChanged,
        attempt: ActiveAttempt,
    ): NativeControl {
        val active = try {
            event.attempt.toDomain(attempt.lifecycle.ownerBareJid)
        } catch (_: IllegalArgumentException) {
            return NativeControl.Fenced
        }
        if (
            !messenger.matches(attempt.handle, active) ||
            !resume.persistResumeSnapshot(active, event.state)
        ) {
            return NativeControl.Fenced
        }
        return NativeControl.Consumed
    }

    private fun handleDomainEvent(
        event: WaddleClientEvent,
        attempt: ActiveAttempt,
    ): NativeControl {
        val domain = try {
            event.toXmppEvent(attempt.lifecycle.ownerBareJid)
        } catch (_: IllegalArgumentException) {
            return NativeControl.Fenced
        } ?: return NativeControl.Fenced
        val eventAttempt = domain.deliveryAttempt()
        return if (eventAttempt != null && !messenger.matches(attempt.handle, eventAttempt)) {
            NativeControl.Fenced
        } else {
            NativeControl.Event(domain)
        }
    }

    private fun XmppEvent.deliveryAttempt(): DeliveryAttemptRef? = when (this) {
        is XmppEvent.SessionReady -> attempt
        is XmppEvent.NativeDeliveryAcked -> attempt
        is XmppEvent.NativeDeliveryFailed -> attempt
        else -> null
    }

    private class PhysicalAttemptState {
        var activation: AttemptActivation? = null
        var client: WaddleClientInterface? = null
        var activeAttempt: ActiveAttempt? = null
        var construction: TransportConstructionClaim? = null
        var attachedConstruction: Boolean = false
    }

    private data class ActiveAttempt(
        val client: WaddleClientInterface,
        val session: WaddleSessionInfo,
        val scope: CoroutineScope,
        val lifecycle: SessionLifecycleRef,
        val handle: ConnectionAttemptHandle,
    ) {
        @Volatile
        var producerQuiesced: Boolean = false
    }

    private class OrderedEventPuller(
        private val scope: CoroutineScope,
        private val nativeClient: WaddleClientInterface,
        private val localEvents: ReceiveChannel<XmppEvent>,
        private val handleNative: suspend (WaddleClientEvent) -> NativeControl,
    ) {
        private var nativePoll: Deferred<WaddleClientEvent>? = null

        suspend fun nextDomain(): DomainPull {
            while (true) {
                when (val pulled = nextOrdered()) {
                    OrderedPull.LocalClosed -> return DomainPull.Fenced
                    is OrderedPull.Local -> return DomainPull.Event(pulled.event)
                    is OrderedPull.Native -> when (val converted = handleNative(pulled.event)) {
                        NativeControl.Consumed -> continue
                        NativeControl.Fenced -> return DomainPull.Fenced
                        is NativeControl.Event -> return DomainPull.Event(converted.event)
                    }
                }
            }
        }

        suspend fun cancelNativePoll(): Boolean {
            val poll = nativePoll ?: return true
            poll.cancel()
            return withTimeoutOrNull(ATTEMPT_TEARDOWN_TIMEOUT_MILLIS) {
                poll.join()
                true
            } == true
        }

        private suspend fun nextOrdered(): OrderedPull {
            val poll = nativePoll ?: scope.async { nativeClient.nextEvent() }.also {
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
    }

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

    private sealed interface Readiness {
        data class Ready(
            val kind: SessionReadyKind,
            val attempt: DeliveryAttemptRef,
        ) : Readiness

        data class AuthenticationStopped(
            val condition: WaddleSaslCondition,
            val disposition: SaslRetryDisposition,
        ) : Readiness

        data object Disconnected : Readiness
        data object Fenced : Readiness
    }
}
