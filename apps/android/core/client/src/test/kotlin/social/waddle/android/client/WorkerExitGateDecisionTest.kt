package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class WorkerExitGateDecisionTest {
    @Test
    fun `A bootstrapping worker exit fences exact lifecycle before admissions open`() {
        val lifecycle = SessionLifecycleRef.create(OWNER)
        val exit = exit(lifecycle, WorkerKind.OUTBOUND_DRAIN, WorkerExitReason.UnexpectedReturn)

        assertEquals(
            WorkerExitGateDecision.Fence(LifecycleFenceCause.WorkerExited(WorkerFence(exit))),
            decideWorkerExitGate(OutboundLifecycleState.Bootstrapping(lifecycle), true, true, exit),
        )
    }

    @Test
    fun `D unexpected return and scope cancellation fence while requested stop only records during closing`() {
        val lifecycle = SessionLifecycleRef.create(OWNER)
        val unexpected = exit(lifecycle, WorkerKind.OUTBOUND_DRAIN, WorkerExitReason.UnexpectedReturn)
        val cancelled = exit(lifecycle, WorkerKind.DELIVERY_TERMINAL, WorkerExitReason.OwnerScopeCancelled)
        val requested = exit(lifecycle, WorkerKind.OUTBOUND_DRAIN, WorkerExitReason.RequestedStop)

        assertTrueFence(OutboundLifecycleState.Open(lifecycle), unexpected)
        assertTrueFence(OutboundLifecycleState.Open(lifecycle), cancelled)
        assertEquals(
            WorkerExitGateDecision.RecordOnly,
            decideWorkerExitGate(OutboundLifecycleState.Closing(lifecycle, null, null), true, true, requested),
        )
        assertTrueFence(OutboundLifecycleState.Open(lifecycle), requested)
    }

    @Test
    fun `E stale kind lifecycle and duplicate callbacks cannot fence replacement`() {
        val replacement = SessionLifecycleRef.create(OWNER)
        val old = SessionLifecycleRef.create(OWNER)
        val oldExit = exit(old, WorkerKind.OUTBOUND_DRAIN, WorkerExitReason.UnexpectedReturn)
        val wrongKind = exit(replacement, WorkerKind.DELIVERY_TERMINAL, WorkerExitReason.UnexpectedReturn)

        assertEquals(WorkerExitGateDecision.Ignore, decideWorkerExitGate(OutboundLifecycleState.Open(replacement), false, true, oldExit))
        assertEquals(WorkerExitGateDecision.Ignore, decideWorkerExitGate(OutboundLifecycleState.Open(replacement), false, true, wrongKind))
        assertEquals(WorkerExitGateDecision.Ignore, decideWorkerExitGate(OutboundLifecycleState.Open(replacement), true, false, wrongKind))
    }

    @Test
    fun `F first worker fence is immutable through sibling and awaited requested exits`() {
        val lifecycle = SessionLifecycleRef.create(OWNER)
        val awaited = WorkerOwnership(lifecycle, WorkerKind.OUTBOUND_DRAIN, WorkerGeneration.random())
        val state = OutboundLifecycleState.Fenced(lifecycle, LifecycleFenceCause.AwaitingRequestedWorkerExit(awaited))
        val sibling = exit(lifecycle, WorkerKind.DELIVERY_TERMINAL, WorkerExitReason.OwnerScopeCancelled)
        val requested = WorkerExit(lifecycle, awaited.generation, awaited.kind, WorkerExitReason.RequestedStop)
        val first = decideWorkerExitGate(state, true, true, sibling) as WorkerExitGateDecision.Fence
        val afterFirst = OutboundLifecycleState.Fenced(lifecycle, first.cause)

        assertEquals(WorkerExitGateDecision.Ignore, decideWorkerExitGate(afterFirst, true, true, requested))
    }

    @Test
    fun `F1 later real sibling callback preserves every field of first exact fence`() {
        val lifecycle = SessionLifecycleRef.create(OWNER)
        val first = exit(lifecycle, WorkerKind.DELIVERY_TERMINAL, WorkerExitReason.UnexpectedReturn)
        val sibling = exit(lifecycle, WorkerKind.OUTBOUND_DRAIN, WorkerExitReason.OwnerScopeCancelled)
        val firstDecision = decideWorkerExitGate(OutboundLifecycleState.Open(lifecycle), true, true, first)
            as WorkerExitGateDecision.Fence
        val fenced = OutboundLifecycleState.Fenced(lifecycle, firstDecision.cause)

        assertEquals(WorkerExitGateDecision.Ignore, decideWorkerExitGate(fenced, true, true, sibling))
        val authoritative = (fenced.cause as LifecycleFenceCause.WorkerExited).fence.exit
        assertEquals(first.lifecycle, authoritative.lifecycle)
        assertEquals(first.generation, authoritative.generation)
        assertEquals(first.kind, authoritative.kind)
        assertEquals(first.reason, authoritative.reason)
        assertEquals(WorkerKind.OUTBOUND_DRAIN, sibling.kind)
        assertEquals(WorkerExitReason.OwnerScopeCancelled, sibling.reason)
    }

    private fun assertTrueFence(state: OutboundLifecycleState, exit: WorkerExit) {
        assertTrue(decideWorkerExitGate(state, true, true, exit) is WorkerExitGateDecision.Fence)
    }

    private fun exit(
        lifecycle: SessionLifecycleRef,
        kind: WorkerKind,
        reason: WorkerExitReason,
    ) = WorkerExit(lifecycle, WorkerGeneration.random(), kind, reason)

    private companion object { const val OWNER = "owner@waddle.test" }
}
