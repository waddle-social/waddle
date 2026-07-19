package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Test

class WorkerRecoverySiblingDecisionTest {
    @Test
    fun `L terminal owner selects the exact drain sibling unless its exact exit is recorded`() {
        val lifecycle = SessionLifecycleRef.create(OWNER)
        val terminal = WorkerOwnership(lifecycle, WorkerKind.DELIVERY_TERMINAL, WorkerGeneration.random())
        val drain = WorkerOwnership(lifecycle, WorkerKind.OUTBOUND_DRAIN, WorkerGeneration.random())

        val decision = decideRecoverySiblingStop(
            failed = terminal,
            terminal = terminal,
            terminalExit = fatalExit(terminal),
            drain = drain,
            drainExit = null,
        ) as RecoverySiblingStopDecision.Stop

        assertEquals(lifecycle, decision.sibling.lifecycle)
        assertEquals(WorkerKind.OUTBOUND_DRAIN, decision.sibling.kind)
        assertEquals(drain.generation, decision.sibling.generation)
        assertEquals(
            RecoverySiblingStopDecision.AlreadyExited,
            decideRecoverySiblingStop(
                terminal,
                terminal,
                fatalExit(terminal),
                drain,
                requestedExit(drain),
            ),
        )
    }

    @Test
    fun `L drain owner selects the exact terminal sibling unless its exact exit is recorded`() {
        val lifecycle = SessionLifecycleRef.create(OWNER)
        val terminal = WorkerOwnership(lifecycle, WorkerKind.DELIVERY_TERMINAL, WorkerGeneration.random())
        val drain = WorkerOwnership(lifecycle, WorkerKind.OUTBOUND_DRAIN, WorkerGeneration.random())

        val decision = decideRecoverySiblingStop(
            failed = drain,
            terminal = terminal,
            terminalExit = null,
            drain = drain,
            drainExit = fatalExit(drain),
        ) as RecoverySiblingStopDecision.Stop

        assertEquals(lifecycle, decision.sibling.lifecycle)
        assertEquals(WorkerKind.DELIVERY_TERMINAL, decision.sibling.kind)
        assertEquals(terminal.generation, decision.sibling.generation)
        assertEquals(
            RecoverySiblingStopDecision.AlreadyExited,
            decideRecoverySiblingStop(
                drain,
                terminal,
                requestedExit(terminal),
                drain,
                fatalExit(drain),
            ),
        )
    }

    @Test
    fun `L wrong generation recorded exit is rejected instead of suppressing exact sibling stop`() {
        val lifecycle = SessionLifecycleRef.create(OWNER)
        val terminal = WorkerOwnership(lifecycle, WorkerKind.DELIVERY_TERMINAL, WorkerGeneration.random())
        val drain = WorkerOwnership(lifecycle, WorkerKind.OUTBOUND_DRAIN, WorkerGeneration.random())
        val staleDrain = WorkerOwnership(lifecycle, WorkerKind.OUTBOUND_DRAIN, WorkerGeneration.random())

        assertEquals(
            RecoverySiblingStopDecision.RecordedExitMismatch(drain, staleDrain),
            decideRecoverySiblingStop(
                failed = terminal,
                terminal = terminal,
                terminalExit = fatalExit(terminal),
                drain = drain,
                drainExit = requestedExit(staleDrain),
            ),
        )
        assertEquals(
            RecoverySiblingStopDecision.UnknownFailedWorker,
            decideRecoverySiblingStop(
                failed = staleDrain,
                terminal = terminal,
                terminalExit = fatalExit(terminal),
                drain = drain,
                drainExit = null,
            ),
        )
    }

    private fun fatalExit(ownership: WorkerOwnership): WorkerExit =
        WorkerExit(
            lifecycle = ownership.lifecycle,
            generation = ownership.generation,
            kind = ownership.kind,
            reason = WorkerExitReason.UnexpectedFailure(WorkerFailureKind.DEPENDENCY_FAILURE),
        )

    private fun requestedExit(ownership: WorkerOwnership): WorkerExit =
        WorkerExit(
            lifecycle = ownership.lifecycle,
            generation = ownership.generation,
            kind = ownership.kind,
            reason = WorkerExitReason.RequestedStop,
        )

    private companion object {
        const val OWNER = "recovery@waddle.test"
    }
}
