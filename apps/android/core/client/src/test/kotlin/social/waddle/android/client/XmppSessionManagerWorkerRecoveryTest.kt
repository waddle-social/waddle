package social.waddle.android.client

import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.supervisorScope
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.coroutines.yield
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.DeliveryJournal
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.client.ffi.WaddleSendMessageOutcome

@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionManagerWorkerRecoveryTest {
    @Test
    fun `Q fatal durable recovery error preserves the old scope for retry`() = runTest {
        val entered = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()
        val terminalSelectorConsumed = CompletableDeferred<Unit>()
        val recoverySelectorConsumed = CompletableDeferred<Unit>()
        val dataStore = FailingPreferencesDataStore()
        val prefs = SessionPrefs(dataStore)
        val factory = FakeClientFactory()
        val manager = XmppSessionManager.withLifecyclePhaseObserver(
            sessionPrefs = prefs,
            clientFactory = factory,
            networkSignal = FakeNetworkSignal(),
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScheduler),
            lifecyclePhaseObserver = OutboundLifecyclePhaseObserver.NONE,
        )
        manager.login(testSessionInfo())
        runCurrent()
        factory.emitReady()
        runCurrent()
        val client = factory.clients.single()
        client.sendOutcome = WaddleSendMessageOutcome.Error
        client.beforeSendReturns = {
            entered.complete(Unit)
            release.await()
        }
        supervisorScope {
            val send = async { manager.sendChatMessage("alice@waddle.test", "fatal") }
            awaitCheckpoint("sendEntered", entered)
            dataStore.installAfterCommitReturnsOnceWhen(
                matches = ::addsActiveTerminalIntent,
            ) {
                terminalSelectorConsumed.complete(Unit)
                throw AssertionError("terminal dependency")
            }
            release.complete(Unit)
            val terminalFailure = runCatching {
                awaitCheckpoint("terminal send", send)
            }.exceptionOrNull()
            assertTrue(terminalFailure is TerminalWorkerCommandFailedException)
        }
        runCurrent()
        awaitCheckpoint("terminal selector", terminalSelectorConsumed)
        val fatal = AssertionError("durable recovery")
        dataStore.installAfterCommitReturnsOnceWhen(
            matches = ::fencesActiveTerminalIntent,
        ) {
            recoverySelectorConsumed.complete(Unit)
            throw fatal
        }
        assertSame(
            fatal,
            runCatching { manager.login(testSessionInfo(sessionId = "recovery-fails")) }.exceptionOrNull(),
        )
        awaitCheckpoint("recovery selector", recoverySelectorConsumed)
        val scopeWrite = CompletableDeferred<Unit>()
        dataStore.installAfterCommitReturnsOnceWhen(::updatesScopeSurvivesLastSeen) {
            scopeWrite.complete(Unit)
        }
        manager.recordDmSeen("scope-survives@waddle.test")
        runCurrent()
        awaitCheckpoint("old scope write", scopeWrite)
        manager.login(testSessionInfo(sessionId = "replacement"))
        runCurrent()
        assertEquals(ConnectionState.Connecting, manager.connectionState.value)
        assertEquals(2, factory.clients.size)
        assertLoggedOutTwice(manager, prefs)
    }

    @Test
    fun `Q replacement login recovers terminal fence before cancelling the old scope`() = runTest {
        val terminalRecordEntered = CompletableDeferred<Unit>()
        val releaseTerminalRecord = CompletableDeferred<Unit>()
        val shutdownFinalizedEntered = CompletableDeferred<Unit>()
        val releaseShutdownFinalized = CompletableDeferred<Unit>()
        val oldScopeWriteEntered = CompletableDeferred<Unit>()
        val oldScopeWriteCancelled = CompletableDeferred<Unit>()
        val oldScopeClientCountAtCancellation = CompletableDeferred<Int>()
        val sendEntered = CompletableDeferred<Unit>()
        val releaseSend = CompletableDeferred<Unit>()
        val dataStore = FailingPreferencesDataStore()
        val prefs = SessionPrefs(dataStore)
        val factory = FakeClientFactory()
        lateinit var manager: XmppSessionManager
        manager = XmppSessionManager.withLifecyclePhaseObserver(
            sessionPrefs = prefs,
            clientFactory = factory,
            networkSignal = FakeNetworkSignal(),
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScheduler),
            lifecyclePhaseObserver = OutboundLifecyclePhaseObserver { phase ->
                if (phase == OutboundLifecyclePhase.SHUTDOWN_OWNER_FINALIZED) {
                    dataStore.installAfterCommitReturnsOnceWhen(::updatesScopeWitnessLastSeen) {
                        oldScopeWriteEntered.complete(Unit)
                        try {
                            awaitCancellation()
                        } finally {
                            oldScopeClientCountAtCancellation.complete(factory.clients.size)
                            oldScopeWriteCancelled.complete(Unit)
                        }
                    }
                    manager.recordDmSeen("scope-witness@waddle.test")
                    yield()
                    awaitCheckpoint("oldScopeWriteEntered", oldScopeWriteEntered)
                    shutdownFinalizedEntered.complete(Unit)
                    releaseShutdownFinalized.await()
                }
            },
        )

        manager.login(testSessionInfo())
        runCurrent()
        factory.emitReady()
        runCurrent()
        val originalClient = factory.clients.single()
        originalClient.sendOutcome = WaddleSendMessageOutcome.Error
        originalClient.beforeSendReturns = {
            sendEntered.complete(Unit)
            releaseSend.await()
        }

        supervisorScope {
            val sending = async {
                manager.sendChatMessage("alice@waddle.test", "fatal during teardown")
            }
            awaitCheckpoint("sendEntered", sendEntered)
            dataStore.installAfterCommitReturnsOnceWhen(::addsActiveTerminalIntent) {
                terminalRecordEntered.complete(Unit)
                releaseTerminalRecord.await()
                throw AssertionError("terminal journal dependency failed")
            }
            releaseSend.complete(Unit)
            awaitCheckpoint("terminalRecordEntered", terminalRecordEntered)

            val replacement = async { manager.login(testSessionInfo(sessionId = "replacement")) }
            runCurrent()
            assertFalse(
                "teardown must await the still-live terminal worker",
                replacement.isCompleted,
            )

            releaseTerminalRecord.complete(Unit)
            awaitCheckpoint("shutdownFinalizedEntered", shutdownFinalizedEntered)
            assertFalse(
                "shutdown has fenced the terminal worker but has not cancelled the old scope yet",
                replacement.isCompleted,
            )
            assertFalse(oldScopeWriteCancelled.isCompleted)
            releaseShutdownFinalized.complete(Unit)
            runCurrent()
            val terminalFailure = runCatching {
                awaitCheckpoint("sending", sending)
            }.exceptionOrNull()
            assertTrue(terminalFailure is TerminalWorkerCommandFailedException)
            awaitCheckpoint("oldScopeWriteCancelled", oldScopeWriteCancelled)
            assertEquals(1, oldScopeClientCountAtCancellation.await())
            awaitCheckpoint("replacement", replacement)
            runCurrent()

            assertEquals(ConnectionState.Connecting, manager.connectionState.value)
            assertEquals(2, factory.clients.size)
            assertEquals("replacement", prefs.sessionId.first())
            assertLoggedOutTwice(manager, prefs)
        }
    }

    private suspend fun awaitCheckpoint(name: String, deferred: CompletableDeferred<Unit>) {
        check(withTimeoutOrNull(5_000) { deferred.await(); true } == true) {
            "timed out waiting for $name"
        }
    }

    private suspend fun <T> awaitCheckpoint(name: String, deferred: kotlinx.coroutines.Deferred<T>): T =
        withTimeoutOrNull(5_000) { deferred.await() }
            ?: error("timed out waiting for $name")

    private suspend fun TestScope.assertLoggedOutTwice(manager: XmppSessionManager, prefs: SessionPrefs) {
        manager.logout()
        runCurrent()
        assertEquals(ConnectionState.Idle, manager.connectionState.value)
        assertEquals(WaddleAppState.SignedOut, manager.appState.value)
        assertEquals(null, prefs.sessionId.first())
        manager.logout()
        runCurrent()
        assertEquals(ConnectionState.Idle, manager.connectionState.value)
        assertEquals(WaddleAppState.SignedOut, manager.appState.value)
        assertEquals(null, prefs.sessionId.first())
    }

    private fun committedJournal(preferences: Preferences): DeliveryJournal? =
        preferences[DELIVERY_JOURNAL_KEY]
            ?.let { TEST_JSON.decodeFromString<DeliveryJournal>(it) }

    private fun updatesScopeSurvivesLastSeen(before: Preferences, after: Preferences): Boolean =
        updatesLastSeen(before, after, "scope-survives@waddle.test")

    private fun updatesScopeWitnessLastSeen(before: Preferences, after: Preferences): Boolean =
        updatesLastSeen(before, after, "scope-witness@waddle.test")

    private fun updatesLastSeen(before: Preferences, after: Preferences, peer: String): Boolean {
        val old = before[LAST_SEEN_KEY]?.let { TEST_JSON.decodeFromString<Map<String, String>>(it) }.orEmpty()
        val next = after[LAST_SEEN_KEY]?.let { TEST_JSON.decodeFromString<Map<String, String>>(it) }.orEmpty()
        return next[peer] != null && next[peer] != old[peer]
    }

    private fun addsActiveTerminalIntent(before: Preferences, after: Preferences): Boolean {
        val old = committedJournal(before)?.owners?.get(OWNER) ?: return false
        val next = committedJournal(after)?.owners?.get(OWNER) ?: return false
        return old.activeAttempt != null && next.activeAttempt == old.activeAttempt &&
            old.terminalIntents.isEmpty() && next.terminalIntents.isNotEmpty()
    }

    private fun fencesActiveTerminalIntent(before: Preferences, after: Preferences): Boolean {
        val old = committedJournal(before)?.owners?.get(OWNER) ?: return false
        val next = committedJournal(after)?.owners?.get(OWNER) ?: return false
        return old.activeAttempt != null && next.activeAttempt == null &&
            old.terminalIntents.isNotEmpty() && next.terminalIntents == old.terminalIntents
    }

    private companion object {
        const val OWNER = "icepuma@waddle.test"
        val DELIVERY_JOURNAL_KEY = stringPreferencesKey("delivery_journal_v1")
        val LAST_SEEN_KEY = stringPreferencesKey("last_seen")
        val TEST_JSON = Json { ignoreUnknownKeys = true }
    }

    @Test
    fun `Q residual worker fence maps through the production shutdown requirement`() {
        val lifecycle = SessionLifecycleRef.create("q@waddle.test")
        val exit = WorkerExit(
            lifecycle = lifecycle,
            generation = WorkerGeneration.random(),
            kind = WorkerKind.DELIVERY_TERMINAL,
            reason = WorkerExitReason.UnexpectedFailure(WorkerFailureKind.DEPENDENCY_FAILURE),
        )
        val cause = LifecycleFenceCause.WorkerExited(WorkerFence(exit))

        val manager = XmppSessionManager.withLifecyclePhaseObserver(
            sessionPrefs = SessionPrefs(InMemoryPreferencesDataStore()),
            clientFactory = FakeClientFactory(),
            networkSignal = FakeNetworkSignal(),
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(),
            lifecyclePhaseObserver = OutboundLifecyclePhaseObserver.NONE,
        )
        val failure = runCatching {
            manager.requireStopped(lifecycle, LifecycleShutdownOutcome.WorkerFenced(lifecycle, cause))
        }.exceptionOrNull() as WorkerRecoveryException
        val recovered = failure.outcome as WorkerRecoveryOutcome.WorkerFenced
        assertEquals(lifecycle, recovered.lifecycle)
        assertSame(cause, recovered.cause)
        val retainedExit = (recovered.cause as LifecycleFenceCause.WorkerExited).fence.exit
        assertSame(exit, retainedExit)
        assertEquals(exit.generation, retainedExit.generation)
        assertEquals(exit.kind, retainedExit.kind)
        assertEquals(exit.reason, retainedExit.reason)
    }

}
