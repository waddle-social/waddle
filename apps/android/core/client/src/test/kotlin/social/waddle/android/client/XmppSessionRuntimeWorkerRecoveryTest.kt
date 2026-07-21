package social.waddle.android.client

import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.supervisorScope
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.coroutines.yield
import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.DeliveryJournal
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.client.ffi.WaddleSendMessageOutcome
import java.io.IOException

@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionRuntimeWorkerRecoveryTest {
    @Test
    fun `E1 manager recovery preserves terminal dependency runtime evidence`() = runTest {
        assertTerminalFailureEvidence(IllegalStateException("terminal dependency runtime"))
    }

    @Test
    fun `E1 manager recovery preserves terminal owner scope cancellation evidence`() = runTest {
        assertTerminalFailureEvidence(kotlinx.coroutines.CancellationException("terminal owner scope cancellation"))
    }

    @Test
    fun `E1 manager recovery preserves terminal assertion evidence`() = runTest {
        assertTerminalFailureEvidence(AssertionError("terminal dependency assertion"))
    }

    @Test
    fun `E1 requested stop racing terminal cancellation is cause-less`() = runTest {
        val sendEntered = CompletableDeferred<Unit>()
        val releaseSend = CompletableDeferred<Unit>()
        val terminalMutationEntered = CompletableDeferred<Unit>()
        val releaseTerminalMutation = CompletableDeferred<Unit>()
        val dataStore = FailingPreferencesDataStore()
        val prefs = SessionPrefs(dataStore)
        val factory = FakeClientFactory()
        val manager = XmppSessionRuntime.withLifecyclePhaseObserver(
            sessionPrefs = prefs,
            clientFactory = factory,
            networkSignal = FakeNetworkSignal(),
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScheduler),
            lifecyclePhaseObserver = OutboundLifecyclePhaseObserver.NONE,
            workerExitEvidence = WorkerExitExceptionEvidence(),
        )
        manager.login(testSessionInfo())
        runCurrent()
        factory.emitReady()
        runCurrent()
        factory.clients.single().apply {
            sendOutcome = WaddleSendMessageOutcome.Error
            beforeSendReturns = {
                sendEntered.complete(Unit)
                releaseSend.await()
            }
        }
        supervisorScope {
            val send = async { manager.sendChatMessage("alice@waddle.test", "requested stop race") }
            awaitCheckpoint("terminal send entered", sendEntered)
            val cancellation = kotlinx.coroutines.CancellationException("terminal cancellation races requested stop")
            dataStore.installAfterCommitReturnsOnceWhen(::addsActiveTerminalIntent) {
                terminalMutationEntered.complete(Unit)
                releaseTerminalMutation.await()
                throw cancellation
            }
            releaseSend.complete(Unit)
            awaitCheckpoint("terminal mutation entered", terminalMutationEntered)

            val replacement = async { manager.login(testSessionInfo(sessionId = "requested-stop-replacement")) }
            runCurrent()
            assertFalse(replacement.isCompleted)
            releaseTerminalMutation.complete(Unit)
            awaitCheckpoint("replacement", replacement)
            assertTrue(
                runCatching { awaitCheckpoint("raced terminal command", send) }.exceptionOrNull()
                    is TerminalWorkerUnavailableException,
            )
        }
        runCurrent()
        assertEquals(ConnectionState.Connecting, manager.connectionState.value)
        assertEquals("requested-stop-replacement", prefs.sessionId.first())
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
        lateinit var manager: XmppSessionRuntime
        manager = XmppSessionRuntime.withLifecyclePhaseObserver(
            sessionPrefs = prefs,
            clientFactory = factory,
            networkSignal = FakeNetworkSignal(),
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScheduler),
            lifecyclePhaseObserver = OutboundLifecyclePhaseObserver { phase ->
                if (phase == OutboundLifecyclePhase.SHUTDOWN_OWNER_FINALIZED) {
                    // This hook runs after A has passed the runtime-generation
                    // fence but before its DataStore mutation becomes durable.
                    // Replacement B must cancel/join A's owned child before B
                    // can activate, so this write cannot land in B's runtime.
                    dataStore.installBeforeCommitReturnsOnce {
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
            workerExitEvidence = WorkerExitExceptionEvidence(),
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
        check(
            withTimeoutOrNull(5_000) {
            deferred.await()
        true
        } == true
        ) {
            "timed out waiting for $name"
        }
    }

    private suspend fun <T> awaitCheckpoint(name: String, deferred: kotlinx.coroutines.Deferred<T>): T =
        withTimeoutOrNull(5_000) { deferred.await() }
            ?: error("timed out waiting for $name")

    private suspend fun TestScope.assertLoggedOutTwice(manager: XmppSessionRuntime, prefs: SessionPrefs) {
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

    private suspend fun TestScope.assertTerminalFailureEvidence(primary: Throwable) {
        val sendEntered = CompletableDeferred<Unit>()
        val releaseSend = CompletableDeferred<Unit>()
        val terminalMutation = CompletableDeferred<Unit>()
        val recoveryMutation = CompletableDeferred<Unit>()
        val dataStore = FailingPreferencesDataStore()
        val prefs = SessionPrefs(dataStore)
        val factory = FakeClientFactory()
        val manager = XmppSessionRuntime.withLifecyclePhaseObserver(
            sessionPrefs = prefs,
            clientFactory = factory,
            networkSignal = FakeNetworkSignal(),
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScheduler),
            lifecyclePhaseObserver = OutboundLifecyclePhaseObserver.NONE,
            workerExitEvidence = WorkerExitExceptionEvidence(),
        )
        manager.login(testSessionInfo())
        runCurrent()
        factory.emitReady()
        runCurrent()
        factory.clients.single().apply {
            sendOutcome = WaddleSendMessageOutcome.Error
            beforeSendReturns = {
                sendEntered.complete(Unit)
                releaseSend.await()
            }
        }
        supervisorScope {
            val send = async { manager.sendChatMessage("alice@waddle.test", "e1 terminal failure") }
            awaitCheckpoint("terminal send entered", sendEntered)
            dataStore.installAfterCommitReturnsOnceWhen(::addsActiveTerminalIntent) {
                terminalMutation.complete(Unit)
                throw primary
            }
            releaseSend.complete(Unit)
            val commandFailure = runCatching { awaitCheckpoint("terminal command", send) }.exceptionOrNull()
            assertTrue(
                "the actual terminal command must fail before manager recovery",
                commandFailure is TerminalWorkerCommandFailedException ||
                    commandFailure is TerminalWorkerUnavailableException,
            )
        }
        awaitCheckpoint("terminal failure mutation", terminalMutation)
        dataStore.installAfterCommitReturnsOnceWhen(::fencesActiveTerminalIntent) {
            recoveryMutation.complete(Unit)
            throw IOException("retain terminal evidence for recovery retry")
        }
        val recovery = runCatching {
            manager.login(testSessionInfo(sessionId = "recovery-retry"))
        }.exceptionOrNull() as? WorkerRecoveryException
        checkNotNull(recovery) { "real manager recovery must surface WorkerRecoveryException" }
        assertSame(primary, recovery.cause)
        assertTrue(recovery.outcome is WorkerRecoveryOutcome.DurableCleanupFailed)
        awaitCheckpoint("durable recovery mutation", recoveryMutation)

        manager.login(testSessionInfo(sessionId = "recovered"))
        runCurrent()
        assertEquals(ConnectionState.Connecting, manager.connectionState.value)
        assertEquals(2, factory.clients.size)
        assertLoggedOutTwice(manager, prefs)
    }
}
