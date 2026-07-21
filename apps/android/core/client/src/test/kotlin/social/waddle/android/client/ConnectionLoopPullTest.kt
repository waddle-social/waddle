package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.ConnectionLoopPullHarness.Companion.OWNER
import social.waddle.android.client.ConnectionLoopPullHarness.Companion.PEER
import social.waddle.android.client.prefs.DeliveryAttemptId
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.NativeOutboundPhase
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.toDomain
import social.waddle.android.client.prefs.toFfi
import social.waddle.android.client.prefs.toSnapshot
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.session.ConnectionAttemptRunner
import social.waddle.android.client.session.ConnectionLoop
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleDeliveryAttemptTransition
import social.waddle.client.ffi.WaddleSendMessageOutcome
import social.waddle.client.ffi.WaddleSessionReadyKind
import java.lang.reflect.Modifier

@OptIn(ExperimentalCoroutinesApi::class)
class ConnectionLoopPullTest {
    @Test
    fun `connect and readiness timeout windows have independent exact boundaries`() = runTest {
        val connectGate = CompletableDeferred<Unit>()
        val connectTimeout = ConnectionLoopPullHarness(this, connectTimeoutMillis = 100)
        connectTimeout.factory.configureClient = { client ->
            client.beforeConnectReturns = { connectGate.await() }
        }
        try {
            connectTimeout.start()
            runCurrent()
            val first = connectTimeout.factory.clients.single()
            advanceTimeBy(99)
            runCurrent()
            assertEquals(0, first.disconnectCalls)
            assertEquals(1, connectTimeout.factory.clients.size)
            advanceTimeBy(1)
            runCurrent()
            assertEquals(1, first.disconnectCalls)
            advanceTimeBy(1_000)
            runCurrent()
            assertEquals(2, connectTimeout.factory.clients.size)
        } finally {
            connectGate.complete(Unit)
            connectTimeout.shutdown()
        }

        val readinessTimeout = ConnectionLoopPullHarness(this, connectTimeoutMillis = 100)
        readinessTimeout.factory.configureClient = { client ->
            client.beforeConnectReturns = { delay(75) }
        }
        try {
            readinessTimeout.start()
            runCurrent()
            val first = readinessTimeout.factory.clients.single()
            advanceTimeBy(75)
            runCurrent()
            assertEquals(0, first.disconnectCalls)
            assertEquals(1, first.nextEventCalls.get())
            advanceTimeBy(99)
            runCurrent()
            assertEquals(0, first.disconnectCalls)
            assertEquals(1, readinessTimeout.factory.clients.size)
            advanceTimeBy(1)
            runCurrent()
            assertEquals(1, first.disconnectCalls)
            advanceTimeBy(1_000)
            runCurrent()
            assertEquals(2, readinessTimeout.factory.clients.size)
        } finally {
            readinessTimeout.shutdown()
        }
    }

    @Test
    fun `injected cancellation rethrows its exact instance and closes only that attempt`() = runTest {
        val connectCancellation = SentinelCancellationException(CancellationPhase.CONNECT)
        val connecting = ConnectionLoopPullHarness(this)
        connecting.factory.configureClient = { client ->
            client.beforeConnectReturns = { throw connectCancellation }
        }
        var connectCompletion: Throwable? = null
        try {
            connecting.start()
            connecting.loopJob.invokeOnCompletion { connectCompletion = it }
            runCurrent()
            val client = connecting.factory.clients.single()
            assertSame(connectCancellation, connectCompletion)
            assertEquals(1, client.disconnectCalls)
            assertEquals(1, connecting.factory.clients.size)
        } finally {
            connecting.shutdown()
        }

        val readinessCancellation = SentinelCancellationException(CancellationPhase.READINESS)
        val awaitingReadiness = ConnectionLoopPullHarness(this)
        awaitingReadiness.factory.configureClient = { client ->
            client.beforeNextEventReturns = { call ->
                if (call == 1) throw readinessCancellation
            }
        }
        var readinessCompletion: Throwable? = null
        try {
            awaitingReadiness.start()
            awaitingReadiness.loopJob.invokeOnCompletion { readinessCompletion = it }
            runCurrent()
            val client = awaitingReadiness.factory.clients.single()
            assertSame(readinessCancellation, readinessCompletion)
            assertEquals(1, client.disconnectCalls)
            assertEquals(1, awaitingReadiness.factory.clients.size)
        } finally {
            awaitingReadiness.shutdown()
        }

        val eventCancellation = SentinelCancellationException(CancellationPhase.EVENT_CONSUMPTION)
        val consumingEvents = ConnectionLoopPullHarness(this)
        consumingEvents.factory.configureClient = { client ->
            client.beforeNextEventReturns = { call ->
                if (call == 2) throw eventCancellation
            }
        }
        var eventCompletion: Throwable? = null
        try {
            consumingEvents.start()
            consumingEvents.loopJob.invokeOnCompletion { eventCompletion = it }
            runCurrent()
            consumingEvents.factory.emitReady()
            runCurrent()
            val client = consumingEvents.factory.clients.single()
            assertSame(eventCancellation, eventCompletion)
            assertEquals(1, client.disconnectCalls)
            assertEquals(1, consumingEvents.factory.clients.size)
        } finally {
            consumingEvents.shutdown()
        }
    }

    @Test
    fun `runner retains immutable collaborators and loop owns retry state`() {
        val runnerFields = ConnectionAttemptRunner::class.java.declaredFields
            .filterNot { Modifier.isStatic(it.modifiers) }
        assertEquals(
            setOf("attemptClientFactory", "resume", "router", "messenger", "connectTimeoutMillis"),
            runnerFields.map { it.name }.toSet(),
        )
        assertTrue(runnerFields.all { Modifier.isFinal(it.modifiers) })
        val forbiddenTypes: Set<Class<*>> = setOf(
            WaddleClientInterface::class.java,
            ConnectionAttemptHandle::class.java,
            Job::class.java,
            Channel::class.java,
            Mutex::class.java,
            ActiveSession::class.java,
            SessionLifecycleRef::class.java,
            Function0::class.java,
            Function1::class.java,
            Function2::class.java,
            Function3::class.java,
            Function4::class.java,
        )
        assertTrue(
            runnerFields.none { field ->
                forbiddenTypes.any { forbidden -> forbidden.isAssignableFrom(field.type) }
            },
        )
        val loopFields = ConnectionLoop::class.java.declaredFields.map { it.name }.toSet()
        assertTrue("attemptRunner" in loopFields)
        assertTrue("retryRequests" in loopFields)
        assertTrue("admissionsOpen" in loopFields)
    }

    @Test
    fun `native pull is serialized with no prefetch`() = runTest {
        val harness = ConnectionLoopPullHarness(this)
        try {
            harness.start()
            runCurrent()
            val client = harness.factory.clients.single()
            assertPulls(client, calls = 1, inFlight = 1)

            harness.factory.emitReady()
            runCurrent()
            assertPulls(client, calls = 2, inFlight = 1)

            harness.factory.emit(WaddleClientEvent.Error("keep consuming"))
            runCurrent()
            assertPulls(client, calls = 3, inFlight = 1)
            assertEquals(1, client.maxInFlightNextEvents.get())
        } finally {
            harness.shutdown()
        }
    }

    @Test
    fun `resume transition commits before the next native pull`() = runTest {
        val harness = ConnectionLoopPullHarness(this)
        try {
            harness.seedResumableRow(RESUME_STANZA_ID)
            harness.start()
            runCurrent()
            val client = harness.factory.clients.single()
            val old = harness.factory.configs.single().deliveryAttempt.toDomain(OWNER)
            val fresh = old.copy(
                attemptId = DeliveryAttemptId(FRESH_ATTEMPT_ID),
                nativeGeneration = old.nativeGeneration.next(),
            )

            harness.dataStore.failAllUpdates = true
            harness.factory.emitResumeFailed(
                WaddleDeliveryAttemptTransition(old.toFfi(), fresh.toFfi()),
                affectedStanzaIds = listOf(RESUME_STANZA_ID),
            )
            runCurrent()
            assertPulls(client, calls = 1, inFlight = 0)
            assertEquals(old, harness.prefs.deliveryJournal.first().owners[OWNER]?.activeAttempt)

            harness.dataStore.failAllUpdates = false
            advanceTimeBy(250)
            runCurrent()
            assertPulls(client, calls = 2, inFlight = 1)
            assertEquals(fresh, harness.prefs.deliveryJournal.first().owners[OWNER]?.activeAttempt)
        } finally {
            harness.shutdown()
        }
    }

    @Test
    fun `stale stop preserves fresh resume drain worker and fifo`() = runTest {
        val harness = ConnectionLoopPullHarness(this)
        try {
            val resume = prepareFreshResume(harness)
            makeFreshAttemptReadyAfterStaleStop(harness, resume)
            val drain = admitFreshFifo(harness, resume.client)
            assertFreshFifoDrain(harness, resume, drain)
        } finally {
            harness.shutdown()
        }
    }

    @Test
    fun `ack and failure effects complete before the next native pull`() = runTest {
        val harness = ConnectionLoopPullHarness(this)
        try {
            harness.start()
            runCurrent()
            harness.factory.emitReady()
            runCurrent()
            val client = harness.factory.clients.single()

            val acked = harness.messenger.sendOrEnqueue(PEER, false, "acked")
            val ackedId = checkNotNull(acked.delivery).identity.clientStanzaId
            harness.dataStore.failAllUpdates = true
            harness.factory.emitAcked(ackedId)
            runCurrent()
            assertPulls(client, calls = 2, inFlight = 0)
            harness.dataStore.failAllUpdates = false
            advanceTimeBy(250)
            runCurrent()
            assertPulls(client, calls = 3, inFlight = 1)

            val failed = harness.messenger.sendOrEnqueue(PEER, false, "failed")
            val failedId = checkNotNull(failed.delivery).identity.clientStanzaId
            harness.dataStore.failAllUpdates = true
            harness.factory.emitFailed(failedId)
            runCurrent()
            assertPulls(client, calls = 3, inFlight = 0)
            harness.dataStore.failAllUpdates = false
            advanceTimeBy(250)
            runCurrent()
            assertPulls(client, calls = 4, inFlight = 1)
        } finally {
            harness.shutdown()
        }
    }

    @Test
    fun `retryable head failure queues a connected compose and ack advances once`() = runTest {
        val harness = ConnectionLoopPullHarness(this)
        try {
            harness.start()
            runCurrent()
            harness.factory.emitReady()
            runCurrent()
            val client = harness.factory.clients.single()
            client.sendOutcomes += WaddleSendMessageOutcome.NotConnected

            val first = harness.messenger.sendOrEnqueue(
                PEER,
                false,
                "first retryable",
            )
            val firstId = checkNotNull(first.delivery).identity.clientStanzaId
            assertEquals(WaddleSendMessageOutcome.NotConnected, first.outcome)
            assertEquals(1, client.sendCalls.size)

            val second = harness.messenger.sendOrEnqueue(
                PEER,
                false,
                "second connected compose",
            )
            val secondId = checkNotNull(second.delivery).identity.clientStanzaId
            assertEquals(WaddleSendMessageOutcome.NotConnected, second.outcome)
            assertEquals(
                listOf(firstId),
                client.sendOptions.map { it?.stanzaId },
            )
            assertEquals(
                listOf(firstId, secondId),
                harness.queue.rows(OWNER).map { it.clientStanzaId },
            )

            runCurrent()
            assertEquals(
                listOf(firstId, firstId),
                client.sendOptions.map { it?.stanzaId },
            )

            harness.factory.emitAcked(firstId)
            runCurrent()
            assertEquals(
                listOf(firstId, firstId, secondId),
                client.sendOptions.map { it?.stanzaId },
            )
            assertEquals(1, client.sendOptions.count { it?.stanzaId == secondId })

            harness.factory.emitAcked(secondId)
            runCurrent()
            assertTrue(harness.queue.rows(OWNER).isEmpty())
        } finally {
            harness.shutdown()
        }
    }

    @Test
    fun `queued admission returns before nonretryable predecessor advances to target`() = runTest {
        val harness = ConnectionLoopPullHarness(this)
        try {
            harness.start()
            runCurrent()
            harness.factory.emitReady()
            runCurrent()
            val client = harness.factory.clients.single()
            client.sendOutcomes += WaddleSendMessageOutcome.NotConnected

            val predecessor = harness.messenger.sendOrEnqueue(
                PEER,
                false,
                "predecessor",
            )
            val predecessorId =
                checkNotNull(predecessor.delivery).identity.clientStanzaId
            client.sendOutcomes += WaddleSendMessageOutcome.Error

            val target = harness.messenger.sendOrEnqueue(
                PEER,
                false,
                "target",
            )
            val targetId = checkNotNull(target.delivery).identity.clientStanzaId

            assertEquals(WaddleSendMessageOutcome.NotConnected, target.outcome)
            assertEquals(
                listOf(predecessorId),
                client.sendOptions.map { it?.stanzaId },
            )

            runCurrent()
            assertEquals(
                listOf(predecessorId, predecessorId, targetId),
                client.sendOptions.map { it?.stanzaId },
            )
            assertEquals(1, client.sendOptions.count { it?.stanzaId == targetId })
            assertEquals(
                listOf(targetId),
                harness.queue.rows(OWNER).map { it.clientStanzaId },
            )
        } finally {
            harness.shutdown()
        }
    }

    @Test
    fun `queued target nonretryable failure emits one exact effect and never reports sent`() = runTest {
        val harness = ConnectionLoopPullHarness(this)
        try {
            harness.start()
            runCurrent()
            harness.factory.emitReady()
            runCurrent()
            val client = harness.factory.clients.single()
            client.sendOutcomes += WaddleSendMessageOutcome.NotConnected

            val predecessor = harness.messenger.sendOrEnqueue(
                PEER,
                false,
                "predecessor fails",
            )
            val predecessorId =
                checkNotNull(predecessor.delivery).identity.clientStanzaId
            client.sendOutcomes += WaddleSendMessageOutcome.Error
            client.sendOutcomes += WaddleSendMessageOutcome.Error

            val target = harness.messenger.sendOrEnqueue(
                PEER,
                false,
                "target fails",
            )
            val targetDelivery = checkNotNull(target.delivery)
            val targetId = targetDelivery.identity.clientStanzaId

            assertEquals(WaddleSendMessageOutcome.NotConnected, target.outcome)
            assertEquals(
                listOf(predecessorId),
                client.sendOptions.map { it?.stanzaId },
            )

            runCurrent()
            assertEquals(
                listOf(predecessorId, predecessorId, targetId),
                client.sendOptions.map { it?.stanzaId },
            )
            assertTrue(harness.queue.rows(OWNER).isEmpty())
            assertEquals(
                1,
                harness.deliveryEvents.count { event ->
                    event == XmppEvent.DeliveryFailed(targetDelivery)
                },
            )
            assertTrue(
                harness.deliveryEvents.none { event ->
                    event == XmppEvent.DeliveryAcked(targetDelivery)
                },
            )
        } finally {
            harness.shutdown()
        }
    }

    @Test
    fun `concurrent queued wakes stay single flight and preserve exact order`() = runTest {
        val harness = ConnectionLoopPullHarness(this)
        try {
            harness.start()
            runCurrent()
            harness.factory.emitReady()
            runCurrent()
            val client = harness.factory.clients.single()
            client.sendOutcomes += WaddleSendMessageOutcome.NotConnected

            val predecessor =
                harness.messenger.sendOrEnqueue(PEER, false, "blocked predecessor")
            val predecessorId =
                checkNotNull(predecessor.delivery).identity.clientStanzaId
            val replayStarted = CompletableDeferred<Unit>()
            val releaseReplay = CompletableDeferred<Unit>()
            var blockReplay = true
            client.beforeSendReturns = {
                if (blockReplay) {
                    blockReplay = false
                    replayStarted.complete(Unit)
                    releaseReplay.await()
                }
            }
            client.sendOutcomes += WaddleSendMessageOutcome.Error

            val second = harness.messenger.sendOrEnqueue(PEER, false, "second")
            val secondId = checkNotNull(second.delivery).identity.clientStanzaId
            runCurrent()
            replayStarted.await()

            val third = async {
                harness.messenger.sendOrEnqueue(PEER, false, "third")
            }
            val fourth = async {
                harness.messenger.sendOrEnqueue(PEER, false, "fourth")
            }
            runCurrent()
            val thirdResult = third.await()
            val fourthResult = fourth.await()
            val thirdId = checkNotNull(thirdResult.delivery).identity.clientStanzaId
            val fourthId = checkNotNull(fourthResult.delivery).identity.clientStanzaId
            assertEquals(WaddleSendMessageOutcome.NotConnected, thirdResult.outcome)
            assertEquals(WaddleSendMessageOutcome.NotConnected, fourthResult.outcome)
            assertEquals(
                listOf(predecessorId, predecessorId),
                client.sendOptions.map { it?.stanzaId },
            )

            releaseReplay.complete(Unit)
            runCurrent()
            assertEquals(1, client.sendOptions.count { it?.stanzaId == secondId })
            assertEquals(0, client.sendOptions.count { it?.stanzaId == thirdId })
            assertEquals(0, client.sendOptions.count { it?.stanzaId == fourthId })

            harness.factory.emitAcked(secondId)
            runCurrent()
            assertEquals(1, client.sendOptions.count { it?.stanzaId == thirdId })
            assertEquals(0, client.sendOptions.count { it?.stanzaId == fourthId })
        } finally {
            harness.shutdown()
        }
    }

    @Test
    fun `live admission storage failure performs no native send`() = runTest {
        val harness = ConnectionLoopPullHarness(this)
        try {
            harness.start()
            runCurrent()
            harness.factory.emitReady()
            runCurrent()
            val client = harness.factory.clients.single()

            harness.dataStore.failNextUpdate = true
            val result = harness.messenger.sendOrEnqueue(
                PEER,
                false,
                "must persist before ffi",
            )

            assertEquals(WaddleSendMessageOutcome.Error, result.outcome)
            assertTrue(client.sendCalls.isEmpty())
            assertTrue(harness.queue.rows(OWNER).isEmpty())
        } finally {
            harness.shutdown()
        }
    }

    @Test
    fun `stale attempts and failed persistence CAS self fence without another pull`() = runTest {
        val stale = ConnectionLoopPullHarness(this)
        try {
            stale.start()
            runCurrent()
            val client = stale.factory.clients.single()
            val expected = stale.factory.configs.single().deliveryAttempt.toDomain(OWNER)
            val wrong = expected.copy(attemptId = DeliveryAttemptId(STALE_ATTEMPT_ID))
            stale.loop.stopAdmissions()
            stale.factory.emit(
                WaddleClientEvent.SessionReady(
                    WaddleSessionReadyKind.FRESH,
                    wrong.toFfi(),
                ),
            )
            runCurrent()
            assertFenced(client, expectedCalls = 1)
        } finally {
            stale.shutdown()
        }

        val failedCas = ConnectionLoopPullHarness(this)
        try {
            failedCas.start()
            runCurrent()
            failedCas.factory.emitReady()
            runCurrent()
            val client = failedCas.factory.clients.single()
            failedCas.queue.beginAttempt(OWNER)
            failedCas.loop.stopAdmissions()
            failedCas.factory.emitResumeStateChanged(testResumeState())
            runCurrent()
            assertFenced(client, expectedCalls = 2)
        } finally {
            failedCas.shutdown()
        }
    }

    @Test
    fun `disconnect and logout join pending pulls with one disconnect`() = runTest {
        val disconnected = ConnectionLoopPullHarness(this)
        var disconnectedClient: FakeWaddleClient? = null
        try {
            disconnected.start()
            runCurrent()
            disconnected.factory.emitReady()
            runCurrent()
            val client = disconnected.factory.clients.single()
            disconnectedClient = client
            disconnected.loop.stopAdmissions()
            disconnected.factory.emit(WaddleClientEvent.Disconnected)
            runCurrent()
            assertFenced(client, expectedCalls = 2)
        } finally {
            disconnected.shutdown()
        }
        val finalDisconnectedClient = checkNotNull(disconnectedClient)
        assertTrue(disconnected.loopJob.isCompleted)
        assertEquals(1, finalDisconnectedClient.disconnectCalls)

        val loggedOut = ConnectionLoopPullHarness(this)
        val client = try {
            loggedOut.start()
            runCurrent()
            loggedOut.factory.emitReady()
            runCurrent()
            loggedOut.factory.clients.single().also {
                assertEquals(1, it.inFlightNextEvents.get())
            }
        } finally {
            loggedOut.shutdown()
        }
        assertFenced(client, expectedCalls = 2)
        assertTrue(loggedOut.loopJob.isCompleted)
    }

    @Test
    fun `old generation event after replacement is dropped`() = runTest {
        val harness = ConnectionLoopPullHarness(this)
        try {
            harness.start()
            runCurrent()
            harness.factory.emitReady()
            runCurrent()
            val oldClient = harness.factory.clients.single()

            harness.factory.emit(WaddleClientEvent.Disconnected)
            runCurrent()
            advanceTimeBy(1_000)
            runCurrent()
            harness.factory.emitReady(attemptIndex = 1)
            runCurrent()
            val newClient = harness.factory.clients[1]

            val message = WaddleClientEvent.Message(
                testMessage(
                    id = "replacement-message",
                    from = "$PEER/phone",
                    to = OWNER,
                ),
            )
            harness.factory.emitAt(0, message)
            runCurrent()
            assertTrue(harness.stores.timelineStore.timeline(PEER).value.isEmpty())
            assertEquals(2, oldClient.nextEventCalls.get())

            harness.factory.emitAt(1, message)
            runCurrent()
            assertEquals(1, harness.stores.timelineStore.timeline(PEER).value.size)
            assertEquals(3, newClient.nextEventCalls.get())
        } finally {
            harness.shutdown()
        }
    }

    private suspend fun TestScope.prepareFreshResume(
        harness: ConnectionLoopPullHarness,
    ): FreshResume {
        harness.prefs.activateSession(OWNER, SESSION_ID)
        val seededAttempt = harness.queue.beginAttempt(OWNER).attempt
        assertTrue(
            harness.queue.saveSmResume(
                seededAttempt,
                version = 1,
                snapshot = testResumeState().toSnapshot(),
            ),
        )
        harness.start()
        runCurrent()
        val client = harness.factory.clients.single()
        val old = harness.factory.configs.single().deliveryAttempt.toDomain(OWNER)
        val fresh = old.copy(
            attemptId = DeliveryAttemptId(FRESH_ATTEMPT_ID),
            nativeGeneration = old.nativeGeneration.next(),
        )
        harness.factory.emitResumeFailed(
            WaddleDeliveryAttemptTransition(old.toFfi(), fresh.toFfi()),
            affectedStanzaIds = emptyList(),
        )
        runCurrent()
        assertPulls(client, calls = 2, inFlight = 1)
        assertFreshAttempt(harness, fresh)
        return FreshResume(client, old, fresh)
    }

    private suspend fun TestScope.makeFreshAttemptReadyAfterStaleStop(
        harness: ConnectionLoopPullHarness,
        resume: FreshResume,
    ) {
        assertEquals(
            LifecycleShutdownOutcome.Stale,
            harness.messenger.shutdown(
                LifecycleShutdownTarget.ExactAttempt(
                    harness.lifecycle,
                    resume.old,
                ),
            ),
        )
        assertFreshAttempt(harness, resume.fresh)
        harness.factory.emit(
            WaddleClientEvent.SessionReady(
                WaddleSessionReadyKind.FRESH,
                resume.fresh.toFfi(),
            ),
        )
        runCurrent()
    }

    private suspend fun admitFreshFifo(
        harness: ConnectionLoopPullHarness,
        client: FakeWaddleClient,
    ): FreshDrain {
        client.sendOutcomes += WaddleSendMessageOutcome.NotConnected
        val predecessor = harness.messenger.sendOrEnqueue(
            PEER,
            false,
            "fresh retryable predecessor",
        )
        val predecessorId =
            checkNotNull(predecessor.delivery).identity.clientStanzaId
        client.sendOutcomes += WaddleSendMessageOutcome.Error
        val target = harness.messenger.sendOrEnqueue(
            PEER,
            false,
            "fresh queued target",
        )
        val targetDelivery = checkNotNull(target.delivery)
        assertEquals(WaddleSendMessageOutcome.NotConnected, target.outcome)
        assertTrue(target.queued)
        assertEquals(
            listOf(predecessorId),
            client.sendOptions.map { it?.stanzaId },
        )
        assertEquals(
            listOf(predecessorId, targetDelivery.identity.clientStanzaId),
            harness.queue.rows(OWNER).map { it.clientStanzaId },
        )
        return FreshDrain(predecessorId, targetDelivery)
    }

    private suspend fun TestScope.assertFreshFifoDrain(
        harness: ConnectionLoopPullHarness,
        resume: FreshResume,
        drain: FreshDrain,
    ) {
        runCurrent()
        val targetId = drain.target.identity.clientStanzaId
        assertEquals(
            listOf(drain.predecessorId, drain.predecessorId, targetId),
            resume.client.sendOptions.map { it?.stanzaId },
        )
        assertEquals(
            listOf(drain.predecessorId, targetId),
            resume.client.sendOptions.drop(1).map { it?.stanzaId },
        )
        assertEquals(1, resume.client.sendOptions.count { it?.stanzaId == targetId })
        val remaining = harness.queue.rows(OWNER).single()
        assertEquals(drain.target.identity, remaining.identity)
        assertEquals(
            OutboundOwnership.NativeOwned(
                attempt = resume.fresh,
                phase = NativeOutboundPhase.FRESH,
            ),
            remaining.ownership,
        )
        assertFreshAttempt(harness, resume.fresh)
    }

    private suspend fun assertFreshAttempt(
        harness: ConnectionLoopPullHarness,
        fresh: DeliveryAttemptRef,
    ) {
        assertEquals(fresh, harness.activeSession.attemptRef)
        assertEquals(
            fresh,
            harness.prefs.deliveryJournal.first().owners[OWNER]?.activeAttempt,
        )
    }

    private fun assertPulls(client: FakeWaddleClient, calls: Int, inFlight: Int) {
        assertEquals(calls, client.nextEventCalls.get())
        assertEquals(inFlight, client.inFlightNextEvents.get())
        assertEquals(1, client.maxInFlightNextEvents.get())
    }

    private fun assertFenced(client: FakeWaddleClient, expectedCalls: Int) {
        assertEquals(expectedCalls, client.nextEventCalls.get())
        assertEquals(0, client.inFlightNextEvents.get())
        assertEquals(1, client.disconnectCalls)
        assertFalse(client.maxInFlightNextEvents.get() > 1)
    }

    private data class FreshResume(
        val client: FakeWaddleClient,
        val old: DeliveryAttemptRef,
        val fresh: DeliveryAttemptRef,
    )

    private data class FreshDrain(
        val predecessorId: String,
        val target: DeliveryOutcomeRef,
    )

    private enum class CancellationPhase {
        CONNECT,
        READINESS,
        EVENT_CONSUMPTION,
    }

    /** A non-copyable cancellation sentinel for completion-cause identity checks. */
    private class SentinelCancellationException(
        phase: CancellationPhase,
    ) : CancellationException(phase.name)

    private companion object {
        const val RESUME_STANZA_ID = "resume-stanza-1"
        const val SESSION_ID = "sess-1"
        const val FRESH_ATTEMPT_ID = "00000000-0000-4000-8000-000000000002"
        const val STALE_ATTEMPT_ID = "00000000-0000-4000-8000-000000000099"
    }
}
