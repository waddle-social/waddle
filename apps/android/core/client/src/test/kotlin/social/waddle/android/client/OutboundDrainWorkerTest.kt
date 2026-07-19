package social.waddle.android.client

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.DeliveryAttemptId
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.NativeConnectionGeneration
import java.util.UUID

@OptIn(ExperimentalCoroutinesApi::class)
class OutboundDrainWorkerTest {
    @Test
    fun `requested stop emits one matching exit`() = runTest {
        val exits = mutableListOf<WorkerExit>()
        val ownership = ownership()
        val run = OutboundDrainWorker { _, _, _ -> }.start(this, ownership, {}, { exits += it })

        runCurrent()
        run.requestStop()
        val outcome = run.awaitExit(1_000)

        assertEquals(
            WorkerExit(ownership.lifecycle, ownership.generation, ownership.kind, WorkerExitReason.RequestedStop),
            (outcome as WorkerAwaitOutcome.Exited).exit,
        )
        assertEquals(listOf(outcome.exit), exits)
    }

    @Test
    fun `owner scope cancellation is distinct from requested stop`() = runTest {
        val ownerJob = Job()
        val scope = CoroutineScope(coroutineContext + ownerJob)
        val ownership = ownership()
        val exits = mutableListOf<WorkerExit>()
        val run = OutboundDrainWorker { _, _, _ -> }.start(scope, ownership, {}, { exits += it })

        ownerJob.cancel()
        runCurrent()

        val exit = (run.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
        assertEquals(
            WorkerExitReason.OwnerScopeCancelled,
            exit.reason,
        )
        assertEquals(listOf(exit), exits)
    }

    @Test
    fun `fatal drain dependency failure exits with exact ownership`() = runTest {
        val ownership = ownership()
        val run = OutboundDrainWorker { _, _, _ -> error("drain exploded") }
            .start(this, ownership, {}, { })
        val handle = ConnectionAttemptHandle.random()
        val attempt = attempt()
        assertTrue(run.bind(handle, attempt))

        run.signal(handle, attempt)
        runCurrent()

        val exit = (run.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
        assertEquals(ownership.lifecycle, exit.lifecycle)
        assertEquals(ownership.generation, exit.generation)
        assertEquals(ownership.kind, exit.kind)
        assertEquals(
            WorkerFailureKind.DEPENDENCY_FAILURE,
            (exit.reason as WorkerExitReason.UnexpectedFailure).kind,
        )
    }

    @Test
    fun `replacement run has a distinct generation and ignores old signals`() = runTest {
        val observed = mutableListOf<ConnectionAttemptHandle>()
        val worker = OutboundDrainWorker { _, handle, _ -> observed += handle }
        val lifecycle = SessionLifecycleRef.create(OWNER)
        val firstOwnership = WorkerOwnership(lifecycle, WorkerKind.OUTBOUND_DRAIN, WorkerGeneration.random())
        val secondOwnership = WorkerOwnership(lifecycle, WorkerKind.OUTBOUND_DRAIN, WorkerGeneration.random())
        val firstExits = mutableListOf<WorkerExit>()
        val first = worker.start(this, firstOwnership, {}, { firstExits += it })
        val firstHandle = ConnectionAttemptHandle.random()
        val attempt = attempt()
        assertTrue(first.bind(firstHandle, attempt))
        first.requestStop()
        assertEquals(
            WorkerExitReason.RequestedStop,
            (first.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit.reason,
        )

        val secondExits = mutableListOf<WorkerExit>()
        val second = worker.start(this, secondOwnership, {}, { secondExits += it })
        val secondHandle = ConnectionAttemptHandle.random()
        assertTrue(second.bind(secondHandle, attempt))
        first.signal(firstHandle, attempt)
        runCurrent()
        assertTrue(observed.isEmpty())
        second.signal(secondHandle, attempt)
        runCurrent()
        assertEquals(listOf(secondHandle), observed)
        assertTrue(firstOwnership.generation != secondOwnership.generation)
        assertEquals(1, firstExits.size)
        second.requestStop()
        assertEquals(
            WorkerExitReason.RequestedStop,
            (second.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit.reason,
        )
        assertEquals(1, secondExits.size)
    }

    private fun ownership(): WorkerOwnership = WorkerOwnership(
        SessionLifecycleRef.create(OWNER),
        WorkerKind.OUTBOUND_DRAIN,
        WorkerGeneration.random(),
    )

    private fun attempt(): DeliveryAttemptRef = DeliveryAttemptRef(
        OWNER,
        DeliveryAttemptId(UUID.randomUUID().toString()),
        NativeConnectionGeneration.initial(),
    )

    private companion object {
        const val OWNER = "alice@waddle.test"
    }
}
