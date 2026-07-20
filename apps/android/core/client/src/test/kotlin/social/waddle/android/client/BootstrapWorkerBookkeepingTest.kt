package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Test

class BootstrapWorkerBookkeepingTest {
    @Test
    fun `partial teardown marks an uninstalled exact terminal ownership only`() {
        val lifecycle = SessionLifecycleRef.create("bootstrap@waddle.test")
        val workers = OwnerWorkers(lifecycle)
        val bookkeeping = BootstrapWorkerBookkeeping()
        val state = OutboundLifecycleState.Bootstrapping(lifecycle)
        bookkeeping.markPartialTeardown(state, workers, workers.terminalOwnership)

        assertEquals(
            BootstrapExitDisposition.ExpectedTeardown,
            bookkeeping.recordPreInstallExit(
                state,
                workers,
                WorkerExit(lifecycle, workers.terminalOwnership.generation, WorkerKind.DELIVERY_TERMINAL, WorkerExitReason.RequestedStop),
            ),
        )
        val other = WorkerOwnership(lifecycle, WorkerKind.DELIVERY_TERMINAL, WorkerGeneration.random())
        assertEquals(
            BootstrapExitDisposition.NotBootstrap,
            bookkeeping.recordPreInstallExit(
                state,
                workers,
                WorkerExit(lifecycle, other.generation, other.kind, WorkerExitReason.RequestedStop),
            ),
        )
    }
}
