package social.waddle.android.client

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.job
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs

@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionRuntimeLifecycleEdgesTest {
    private class Harness(
        testScope: TestScope,
        phaseObserver: OutboundLifecyclePhaseObserver = OutboundLifecyclePhaseObserver.NONE,
    ) {
        val factory = FakeClientFactory()
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val manager = XmppSessionRuntime.withLifecyclePhaseObserver(
            sessionPrefs = prefs,
            clientFactory = factory,
            networkSignal = FakeNetworkSignal(),
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScope.testScheduler),
            lifecyclePhaseObserver = phaseObserver,
            workerExitEvidence = WorkerExitExceptionEvidence(),
        )
    }

    @Test
    fun `generation overflow leaves the live runtime authoritative`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emitReady()
        runCurrent()
        val existingClient = harness.factory.clients.single()
        setNextGeneration(harness.manager, Long.MAX_VALUE)

        assertTrue(
            runCatching { harness.manager.login(testSessionInfo(sessionId = "overflow-must-not-start")) }
                .exceptionOrNull() is IllegalStateException,
        )
        assertEquals(ConnectionState.Ready, harness.manager.connectionState.value)
        assertEquals(WaddleAppState.Ready, harness.manager.appState.value)
        assertEquals("sess-1", harness.prefs.sessionId.first())
        assertEquals(1, harness.factory.clients.size)
        assertEquals(0, existingClient.disconnectCalls)
        assertTrue(coroutineContext.job.isActive)
        harness.manager.logout()
    }

    @Test
    fun `close retries a failed teardown before becoming terminal`() = runTest {
        val closeFailure = IllegalStateException("first close teardown failure")
        var failFirstShutdown = true
        val harness = Harness(
            this,
            OutboundLifecyclePhaseObserver { phase ->
                if (failFirstShutdown && phase == OutboundLifecyclePhase.SHUTDOWN_OWNER_FINALIZED) {
                    failFirstShutdown = false
                    throw closeFailure
                }
            },
        )
        harness.manager.login(testSessionInfo())
        runCurrent()

        assertSame(closeFailure, runCatching { harness.manager.close() }.exceptionOrNull())
        assertTrue(coroutineContext.job.isActive)
        assertTrue(
            runCatching { harness.manager.login(testSessionInfo(sessionId = "fenced")) }
                .exceptionOrNull() is IllegalStateException,
        )
        harness.manager.close()
        assertTrue(runtimeRootJob(harness.manager).isCompleted)
        assertTrue(coroutineContext.job.isActive)
        assertTrue(runCatching { harness.manager.login(testSessionInfo()) }.exceptionOrNull() is IllegalStateException)
    }

    private fun runtimeRootJob(runtime: XmppSessionRuntime): Job =
        XmppSessionRuntime::class.java.getDeclaredField("runtimeRootJob")
            .apply { isAccessible = true }
            .get(runtime) as Job

    private fun setNextGeneration(runtime: XmppSessionRuntime, value: Long) {
        XmppSessionRuntime::class.java.getDeclaredField("nextGeneration")
            .apply { isAccessible = true }
            .setLong(runtime, value)
    }
}
