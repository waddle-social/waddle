package social.waddle.android.client

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.DeliveryJournalMutation
import social.waddle.android.client.prefs.DeliveryOwnerJournal
import social.waddle.android.client.prefs.ProcessEpoch
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.session.ResumePersistence
import social.waddle.android.client.store.SessionStores

@OptIn(ExperimentalCoroutinesApi::class)
class WorkerExitEvidencePipelineTest {
    @Test
    fun `terminal sentinel travels worker callback through messenger fence and manager exception then disposes on recovery`() = runTest {
        val evidence = PipelineEvidence()
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        prefs.activateSession(TERMINAL_WORKER_OWNER, "pipeline")
        val queue = OutboundQueue(prefs)
        val receipt = pendingTerminalReceipt(TERMINAL_WORKER_OWNER, "pipeline-terminal")
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = TERMINAL_WORKER_OWNER,
                    owners = journal.owners + (TERMINAL_WORKER_OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                ),
                Unit,
            )
        }
        val resume = ResumePersistence(prefs, queue)
        resume.start(backgroundScope)
        val sentinel = IllegalStateException("terminal pipeline sentinel")
        val messenger = OutboundMessenger(
            activeSession = ActiveSession().also { it.ownBareJid = TERMINAL_WORKER_OWNER },
            stores = SessionStores(),
            journal = queue,
            resume = resume,
            dispatchEvent = { throw sentinel },
            workerExitEvidence = evidence,
        )
        val lifecycle = messenger.start(backgroundScope, TERMINAL_WORKER_OWNER).lifecycle
        runCurrent()
        val outcome = messenger.shutdown(LifecycleShutdownTarget.CurrentOwner(lifecycle))
            as LifecycleShutdownOutcome.WorkerFenced
        val manager = XmppSessionManager.withLifecyclePhaseObserver(
            sessionPrefs = SessionPrefs(InMemoryPreferencesDataStore()),
            clientFactory = FakeClientFactory(),
            networkSignal = FakeNetworkSignal(),
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScheduler),
            lifecyclePhaseObserver = OutboundLifecyclePhaseObserver.NONE,
            workerExitEvidence = evidence,
        )

        val first = runCatching { manager.requireStopped(lifecycle, outcome) }.exceptionOrNull() as WorkerRecoveryException
        val repeated = runCatching { manager.requireStopped(lifecycle, outcome) }.exceptionOrNull() as WorkerRecoveryException
        assertSame(sentinel, first.cause)
        assertSame(sentinel, repeated.cause)
        assertTrue((first.outcome as WorkerRecoveryOutcome.WorkerFenced).cause is LifecycleFenceCause.WorkerExited)

        assertTrue(messenger.recoverFencedWorkers(lifecycle) is WorkerRecoveryOutcome.Recovered)
        assertNull((runCatching { manager.requireStopped(lifecycle, outcome) }.exceptionOrNull() as WorkerRecoveryException).cause)
    }

    @Test
    fun `drain sentinel travels messenger worker callback into the same manager evidence owner`() = runTest {
        val evidence = PipelineEvidence()
        val prefs = SessionPrefs(FailingPreferencesDataStore())
        prefs.activateSession(TERMINAL_WORKER_OWNER, "pipeline-drain")
        val queue = OutboundQueue(prefs)
        val resume = ResumePersistence(prefs, queue)
        resume.start(backgroundScope)
        val sentinel = IllegalArgumentException("drain pipeline sentinel")
        val messenger = OutboundMessenger(
            activeSession = ActiveSession().also { it.ownBareJid = TERMINAL_WORKER_OWNER },
            stores = SessionStores(),
            journal = queue,
            resume = resume,
            dispatchEvent = {},
            workerExitEvidence = evidence,
            outboundDrain = { _, _, _ -> throw sentinel },
        )
        val lifecycle = (messenger.start(backgroundScope, TERMINAL_WORKER_OWNER) as LifecycleStartResult.Started).lifecycle
        val attempt = messenger.activateAttempt(lifecycle).bootstrap.attempt
        assertEquals(DrainSignalOutcome.Accepted, messenger.signalDrain(attempt))
        runCurrent()
        val outcome = messenger.shutdown(LifecycleShutdownTarget.CurrentOwner(lifecycle)) as LifecycleShutdownOutcome.WorkerFenced
        val manager = manager(evidence)
        assertSame(sentinel, (runCatching { manager.requireStopped(lifecycle, outcome) }.exceptionOrNull() as WorkerRecoveryException).cause)
        assertTrue(messenger.recoverFencedWorkers(lifecycle) is WorkerRecoveryOutcome.Recovered)
    }

    private fun TestScope.manager(evidence: WorkerExitEvidence): XmppSessionManager =
        XmppSessionManager.withLifecyclePhaseObserver(
            sessionPrefs = SessionPrefs(InMemoryPreferencesDataStore()),
            clientFactory = FakeClientFactory(),
            networkSignal = FakeNetworkSignal(),
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScheduler),
            lifecyclePhaseObserver = OutboundLifecyclePhaseObserver.NONE,
            workerExitEvidence = evidence,
        )

    private class PipelineEvidence : WorkerExitEvidence {
        private val delegate = WorkerExitExceptionEvidence
        override fun record(ownership: WorkerOwnership, failure: Throwable) = delegate.record(ownership, failure)
        override fun discard(ownership: WorkerOwnership) = delegate.discard(ownership)
        override fun lookup(outcome: WorkerRecoveryOutcome): Throwable? = delegate.lookup(outcome)
    }
}
