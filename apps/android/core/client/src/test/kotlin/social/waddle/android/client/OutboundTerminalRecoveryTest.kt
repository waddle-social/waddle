package social.waddle.android.client

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.OutboundQueue.LiveAdmissionResult
import social.waddle.android.client.prefs.DeliveryAttemptId
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryCallbackRef
import social.waddle.android.client.prefs.DeliveryJournalMutation
import social.waddle.android.client.prefs.DeliveryOwnerBareJid
import social.waddle.android.client.prefs.DeliveryOwnerJournal
import social.waddle.android.client.prefs.DeliverySource
import social.waddle.android.client.prefs.DeliveryTerminalKind
import social.waddle.android.client.prefs.NativeConnectionGeneration
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.ProcessEpoch
import social.waddle.android.client.prefs.QueuedOutboundContent
import social.waddle.android.client.prefs.QueuedOutboundDraft
import social.waddle.android.client.prefs.QueuedOutboundPayload
import social.waddle.android.client.prefs.QueuedOutboundTarget
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.TerminalReceipt
import social.waddle.android.client.prefs.TerminalReceiptClaimState
import social.waddle.android.client.prefs.TerminalReceiptEffect
import social.waddle.android.client.prefs.TerminalReceiptId
import social.waddle.android.client.prefs.TerminalReceiptState
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.session.ResumePersistence
import java.util.UUID

@OptIn(ExperimentalCoroutinesApi::class)
class OutboundTerminalRecoveryTest {
    @Test
    fun `receipt release failure stays fenced until recovery releases the exact lease`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        prefs.activateSession(OWNER, "receipt-cleanup")
        val queue = OutboundQueue(prefs)
        val receipt = pendingReceipt("recovery-receipt")
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = OWNER,
                    owners = journal.owners + (OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                ),
                Unit,
            )
        }
        val resume = ResumePersistence(prefs, queue)
        resume.start(backgroundScope)
        val ownerJob = Job()
        val ownerScope = CoroutineScope(coroutineContext + ownerJob)
        val events = mutableListOf<XmppEvent>()
        var failDispatch = true
        val coordinator = OutboundLifecycleCoordinator(
            activeSession = ActiveSession().also { it.ownBareJid = OWNER },
            journal = queue,
            resume = resume,
            dispatchEvent = { event ->
                events += event
                if (failDispatch) {
                    store.failNextUpdate = true
                    error("dispatch failed after the receipt claim")
                }
            },
            drain = { _, _, _ -> },
            transitionTimeoutMillis = TEST_TIMEOUT_MILLIS,
        )

        val failed = coordinator.start(ownerScope, OWNER) as LifecycleStartResult.Failed
        val fenced = coordinator.shutdown(LifecycleShutdownTarget.CurrentOwner(failed.lifecycle))
            as LifecycleShutdownOutcome.WorkerFenced
        val exit = (fenced.cause as LifecycleFenceCause.WorkerExited).fence.exit
        val workerFailure = (exit.reason as WorkerExitReason.UnexpectedFailure).kind
            as WorkerFailureKind.TERMINAL_RECEIPT_APPLICATION
        val cleanup = workerFailure.failure as TerminalReceiptApplicationFailure.CleanupUnresolved
        assertEquals(TerminalReceiptOperation.RELEASE, cleanup.evidence.operation)
        assertEquals(TerminalReceiptCleanupFailureCategory.IO_FAILURE, cleanup.evidence.category)
        assertEquals(1, events.size)

        store.failAllUpdates = true
        val pending = coordinator.recoverFencedWorkers(failed.lifecycle)
            as WorkerRecoveryOutcome.TerminalReceiptCleanupFailed
        assertEquals(cleanup.evidence, pending.cleanup)
        assertEquals(1, events.size)
        store.failAllUpdates = false
        assertEquals(WorkerRecoveryOutcome.Recovered, coordinator.recoverFencedWorkers(failed.lifecycle))
        assertEquals(1, events.size)
        assertTrue(
            (prefs.deliveryJournal.first().owners.getValue(OWNER).terminalReceipt?.state as
                TerminalReceiptState.Pending).claim is TerminalReceiptClaimState.Unclaimed,
        )

        failDispatch = false
        val replacement = (coordinator.start(ownerScope, OWNER) as LifecycleStartResult.Started).lifecycle
        assertEquals(2, events.size)
        assertEquals(
            LifecycleShutdownOutcome.Stopped,
            coordinator.shutdown(LifecycleShutdownTarget.CurrentOwner(replacement)),
        )
        ownerJob.cancelAndJoin()
    }

    @Test
    fun `J fatal terminal exit rejects late command releases lease and recovers replacement`() = runTest {
        val dataStore = FailingPreferencesDataStore()
        val prefs = SessionPrefs(dataStore)
        prefs.activateSession(OWNER, "terminal-fatal")
        val queue = OutboundQueue(prefs)
        val resume = ResumePersistence(prefs, queue)
        resume.start(backgroundScope)
        val ownerJob = Job()
        val ownerScope = CoroutineScope(coroutineContext + ownerJob)
        val activeSession = ActiveSession().also { it.ownBareJid = OWNER }
        val drainEntered = CompletableDeferred<Unit>()
        val releaseDrain = CompletableDeferred<Unit>()
        var fatalDispatch = true
        val delivered = mutableListOf<XmppEvent>()
        val coordinator = OutboundLifecycleCoordinator(
            activeSession = activeSession,
            journal = queue,
            resume = resume,
            dispatchEvent = { event ->
                if (fatalDispatch) error("terminal dispatch dependency failed")
                delivered += event
            },
            drain = { _, _, _ ->
                drainEntered.complete(Unit)
                releaseDrain.await()
            },
            transitionTimeoutMillis = TEST_TIMEOUT_MILLIS,
        )
        val lifecycle = (coordinator.start(ownerScope, OWNER) as LifecycleStartResult.Started).lifecycle
        val activation = coordinator.activate(lifecycle)
        val attempt = activation.bootstrap.attempt
        stageTerminalRow(queue, attempt, "fatal-terminal")
        val admitted = checkNotNull(coordinator.acquireTerminal(attempt))

        coordinator.signalDrain(attempt)
        drainEntered.await()
        val admittedCommand = async {
            coordinator.submitTerminal(OWNER, "fatal-terminal", attempt, DeliveryTerminalKind.ACK)
        }
        runCurrent()
        assertEquals(
            TerminalCommandOutcome.Failed(TerminalWorkerFailure(WorkerFailureKind.DEPENDENCY_FAILURE)),
            admittedCommand.await(),
        )

        val fenced = coordinator.shutdown(LifecycleShutdownTarget.CurrentOwner(lifecycle))
            as LifecycleShutdownOutcome.WorkerFenced
        val cause = fenced.cause as LifecycleFenceCause.WorkerExited
        val exit = cause.fence.exit
        assertEquals(lifecycle, fenced.lifecycle)
        assertEquals(lifecycle, exit.lifecycle)
        assertEquals(WorkerKind.DELIVERY_TERMINAL, exit.kind)
        assertEquals(
            WorkerExitReason.UnexpectedFailure(WorkerFailureKind.DEPENDENCY_FAILURE),
            exit.reason,
        )

        assertNull(coordinator.acquireTerminal(attempt))
        assertEquals(
            TerminalCommandOutcome.WorkerUnavailable,
            coordinator.submitTerminal(OWNER, "late-terminal", attempt, DeliveryTerminalKind.ACK),
        )
        requireLifecycleRelease(
            coordinator.releaseAdmission(admitted),
            admitted.capability,
            LifecycleReleaseSite.TERMINAL_COMMAND,
        )

        val recovery = async { coordinator.recoverFencedWorkers(lifecycle) }
        runCurrent()
        val losingRecovery = coordinator.recoverFencedWorkers(lifecycle)
            as WorkerRecoveryOutcome.RecoveryInProgress
        assertEquals(lifecycle, losingRecovery.claim.lifecycle)
        assertEquals(cause.fence, losingRecovery.claim.fence)
        assertEquals(exit.ownership(), losingRecovery.claim.fence.exit.ownership())

        releaseDrain.complete(Unit)
        runCurrent()
        assertEquals(WorkerRecoveryOutcome.Recovered, recovery.await())
        assertEquals(WorkerRecoveryOutcome.NotFenced, coordinator.recoverFencedWorkers(lifecycle))
        assertTrue(ownerJob.isActive)

        fatalDispatch = false
        val replacement = (coordinator.start(ownerScope, OWNER) as LifecycleStartResult.Started).lifecycle
        assertTrue(replacement != lifecycle)
        val replacementActivation = coordinator.activate(replacement)
        val replacementAttempt = replacementActivation.bootstrap.attempt
        stageTerminalRow(queue, replacementAttempt, "replacement-terminal")
        val replacementLease = checkNotNull(coordinator.acquireTerminal(replacementAttempt))
        assertEquals(
            TerminalCommandOutcome.Committed,
            coordinator.submitTerminal(
                OWNER,
                "replacement-terminal",
                replacementAttempt,
                DeliveryTerminalKind.ACK,
            ),
        )
        requireLifecycleRelease(
            coordinator.releaseAdmission(replacementLease),
            replacementLease.capability,
            LifecycleReleaseSite.TERMINAL_COMMAND,
        )
        assertEquals(1, delivered.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertTrue(ownerJob.isActive)
        ownerJob.cancelAndJoin()
    }

    @Test
    fun `persistent terminal failure fences ordinary restart until explicit recovery`() = runTest {
        val harness = ConnectionLoopPullHarness(this)
        try {
            harness.start()
            runCurrent()
            harness.factory.emitReady()
            runCurrent()
            val client = harness.factory.clients.single()

            harness.dataStore.failAllUpdates = true
            harness.factory.emitResumeStateChanged(testResumeState())
            runCurrent()
            advanceTimeBy(250)
            runCurrent()
            assertPulls(client, calls = 2, inFlight = 0)
            assertEquals(0, client.disconnectCalls)

            harness.dataStore.failAllUpdates = false
            advanceTimeBy(500)
            runCurrent()
            assertPulls(client, calls = 3, inFlight = 1)

            val sent = harness.messenger.sendOrEnqueue(PEER, false, "persist forever")
            val stanzaId = checkNotNull(sent.delivery).identity.clientStanzaId
            val activeAttempt = checkNotNull(
                harness.queue.activeAttempt(ConnectionLoopPullHarness.OWNER),
            )
            harness.dataStore.failAllUpdates = true
            harness.factory.emitAcked(stanzaId)
            runCurrent()
            assertEquals(0, client.disconnectCalls)

            val stopping = async { harness.stopTerminalWorker() }
            runCurrent()
            advanceTimeBy(30_000)
            harness.dataStore.failAllUpdates = false
            runCurrent()
            val result = stopping.await() as LifecycleShutdownOutcome.WorkerFenced
            val awaiting = result.cause as LifecycleFenceCause.AwaitingRequestedWorkerExit
            assertEquals(harness.lifecycle, result.lifecycle)
            assertEquals(harness.lifecycle, awaiting.ownership.lifecycle)
            assertEquals(WorkerKind.DELIVERY_TERMINAL, awaiting.ownership.kind)
            assertEquals(
                activeAttempt,
                harness.queue.activeAttempt(ConnectionLoopPullHarness.OWNER),
            )

            val fencedLifecycle = harness.lifecycle
            assertTrue(
                "ordinary restart must remain fenced while terminal intents are pending",
                runCatching { harness.startReplacementLifecycle() }.isFailure,
            )
            assertEquals(
                WorkerRecoveryOutcome.WorkerExitPending(fencedLifecycle, awaiting.ownership),
                harness.recoverFencedWorkers(fencedLifecycle),
            )
            advanceTimeBy(30_000)
            runCurrent()
            assertEquals(WorkerRecoveryOutcome.Recovered, harness.recoverFencedWorkers(fencedLifecycle))
            assertEquals(WorkerRecoveryOutcome.NotFenced, harness.recoverFencedWorkers(fencedLifecycle))
            val replacement = harness.startReplacementLifecycle()
            assertTrue(replacement != fencedLifecycle)
            runCurrent()
            assertEquals(
                LifecycleShutdownOutcome.Stopped,
                harness.stopReplacementLifecycle(),
            )
        } finally {
            harness.shutdown()
        }
    }

    private fun assertPulls(
        client: FakeWaddleClient,
        calls: Int,
        inFlight: Int,
    ) {
        assertEquals(calls, client.nextEventCalls.get())
        assertEquals(inFlight, client.inFlightNextEvents.get())
        assertEquals(1, client.maxInFlightNextEvents.get())
    }

    private suspend fun stageTerminalRow(
        queue: OutboundQueue,
        attempt: social.waddle.android.client.prefs.DeliveryAttemptRef,
        clientStanzaId: String,
    ) {
        assertTrue(
            queue.enqueueAndClaimAbsoluteHead(
                QueuedOutboundDraft.create(
                    ownerBareJid = OWNER,
                    clientStanzaId = clientStanzaId,
                    enqueuedAtMillis = 1_000,
                    payload = QueuedOutboundPayload(
                        target = QueuedOutboundTarget.Chat(PEER),
                        content = QueuedOutboundContent(clientStanzaId),
                    ),
                    source = DeliverySource.Composer,
                ),
                attempt,
            ) is LiveAdmissionResult.Claimed,
        )
    }

    private fun pendingReceipt(seed: String): TerminalReceipt {
        val attempt = DeliveryAttemptRef(
            ownerBareJid = OWNER,
            attemptId = DeliveryAttemptId(uuid("$seed-attempt")),
            nativeGeneration = NativeConnectionGeneration(1u),
        )
        val row = QueuedOutboundDraft.create(
            ownerBareJid = OWNER,
            clientStanzaId = "$seed-row",
            enqueuedAtMillis = 1,
            payload = QueuedOutboundPayload(
                target = QueuedOutboundTarget.Chat(PEER),
                content = QueuedOutboundContent(seed),
            ),
        ).persisted(1, OutboundOwnership.Ready)
        return TerminalReceipt(
            owner = DeliveryOwnerBareJid(OWNER),
            attempt = attempt,
            id = TerminalReceiptId(uuid("$seed-id")),
            originProcessEpoch = ProcessEpoch(uuid("$seed-origin")),
            preparedAtMillis = 1,
            state = TerminalReceiptState.Pending(
                TerminalReceiptClaimState.Unclaimed,
                listOf(TerminalReceiptEffect.Acknowledged(DeliveryCallbackRef(row.identity, attempt), row)),
            ),
        )
    }

    private fun uuid(seed: String): String = UUID.nameUUIDFromBytes(seed.toByteArray()).toString()

    private companion object {
        const val PEER = "alice@waddle.test"
        const val OWNER = ConnectionLoopPullHarness.OWNER
        const val TEST_TIMEOUT_MILLIS = 100L
    }
}
