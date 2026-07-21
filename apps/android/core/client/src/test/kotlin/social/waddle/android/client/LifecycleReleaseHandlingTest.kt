package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.DeliveryAttemptId
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.NativeConnectionGeneration
import java.util.UUID

class LifecycleReleaseHandlingTest {
    @Test
    fun `I released and documented superseded construction retry are accepted`() {
        val lifecycle = SessionLifecycleRef.create(OWNER)
        val registry = LifecycleOperationRegistry(lifecycle)
        val capability = checkNotNull(registry.issue(attempt()))

        requireLifecycleRelease(
            registry.release(capability),
            capability,
            LifecycleReleaseSite.TRANSPORT_SUPERSEDED,
        )
        requireLifecycleRelease(
            registry.release(capability),
            capability,
            LifecycleReleaseSite.TRANSPORT_SUPERSEDED,
        )
        assertEquals(0, registry.retainedCount())
    }

    @Test
    fun `I live outbound cannot opt into superseded construction retry`() {
        val lifecycle = SessionLifecycleRef.create(OWNER)
        val registry = LifecycleOperationRegistry(lifecycle)
        val capability = checkNotNull(registry.issue(attempt()))
        assertEquals(LifecycleReleaseOutcome.Released, registry.release(capability))

        val violation = expectViolation {
            requireLifecycleRelease(
                registry.release(capability),
                capability,
                LifecycleReleaseSite.LIVE_OUTBOUND,
            )
        }

        assertEquals(LifecycleReleaseOutcome.AlreadyReleased, violation.outcome)
        assertEquals(LifecycleReleaseSite.LIVE_OUTBOUND, violation.site)
        assertEquals(capability.lifecycle, violation.lifecycle)
        assertEquals(capability.attempt, violation.attempt)
        assertEquals(capability.operationId, violation.operationId)
    }

    @Test
    fun `I not owned without primary surfaces exact typed release violation`() {
        val lifecycle = SessionLifecycleRef.create(OWNER)
        val registry = LifecycleOperationRegistry(lifecycle)
        val issued = checkNotNull(registry.issue(attempt()))
        val forged = valueEqualUnissued(issued)

        val violation = expectViolation {
            requireLifecycleRelease(
                registry.release(forged),
                forged,
                LifecycleReleaseSite.TERMINAL_COMMAND,
            )
        }

        assertEquals(LifecycleReleaseOutcome.NotOwned, violation.outcome)
        assertEquals(LifecycleReleaseSite.TERMINAL_COMMAND, violation.site)
        assertEquals(issued.lifecycle, violation.lifecycle)
        assertEquals(issued.attempt, violation.attempt)
        assertEquals(issued.operationId, violation.operationId)
        assertTrue(registry.owns(issued))
        assertEquals(LifecycleReleaseOutcome.Released, registry.release(issued))
    }

    @Test
    fun `I not owned release preserves cancellation primary and attaches typed violation`() {
        val lifecycle = SessionLifecycleRef.create(OWNER)
        val registry = LifecycleOperationRegistry(lifecycle)
        val issued = checkNotNull(registry.issue(attempt()))
        val primary = CancellationException("cancelled primary")

        val thrown = expectPrimary(primary) {
            requireLifecycleRelease(
                LifecycleReleaseOutcome.NotOwned,
                valueEqualUnissued(issued),
                LifecycleReleaseSite.OUTBOUND_DRAIN,
                primary = primary,
            )
        }

        assertSame(primary, thrown)
        assertReleaseViolation(
            thrown.suppressed.single() as LifecycleReleaseViolation,
            LifecycleReleaseOutcome.NotOwned,
            LifecycleReleaseSite.OUTBOUND_DRAIN,
            issued,
        )
        assertEquals(LifecycleReleaseOutcome.Released, registry.release(issued))
    }

    @Test
    fun `I not owned release preserves Error primary and attaches typed violation`() {
        val lifecycle = SessionLifecycleRef.create(OWNER)
        val registry = LifecycleOperationRegistry(lifecycle)
        val issued = checkNotNull(registry.issue(attempt()))
        val primary = AssertionError("programmer error")

        val thrown = expectPrimary(primary) {
            requireLifecycleRelease(
                LifecycleReleaseOutcome.NotOwned,
                valueEqualUnissued(issued),
                LifecycleReleaseSite.ROTATION_MUTATION,
                primary = primary,
            )
        }

        assertSame(primary, thrown)
        assertReleaseViolation(
            thrown.suppressed.single() as LifecycleReleaseViolation,
            LifecycleReleaseOutcome.NotOwned,
            LifecycleReleaseSite.ROTATION_MUTATION,
            issued,
        )
        assertEquals(LifecycleReleaseOutcome.Released, registry.release(issued))
    }

    @Test
    fun `I stale rotation release is rejected without replacing current exact lease`() {
        val lifecycle = SessionLifecycleRef.create(OWNER)
        val registry = LifecycleOperationRegistry(lifecycle)
        val old = attempt()
        val currentCapability = checkNotNull(registry.issue(old))
        val current = RotationMutationLease.issue(
            handle = ConnectionAttemptHandle.random(),
            fresh = old.copy(
                attemptId = DeliveryAttemptId.random(),
                nativeGeneration = old.nativeGeneration.next(),
            ),
            capability = currentCapability,
        )
        val staleCapability = checkNotNull(registry.issue(attempt()))

        val stale = decideRotationMutationRelease(current, staleCapability)
        assertTrue(stale is RotationMutationReleaseDecision.NotOwned)
        assertSame(current, (stale as RotationMutationReleaseDecision.NotOwned).current)
        val exact = decideRotationMutationRelease(current, currentCapability)
        assertTrue(exact is RotationMutationReleaseDecision.ReleaseCurrent)
        assertSame(current, (exact as RotationMutationReleaseDecision.ReleaseCurrent).current)
        assertTrue(registry.owns(currentCapability))
        assertTrue(registry.owns(staleCapability))
        assertEquals(LifecycleReleaseOutcome.Released, registry.release(currentCapability))
        assertEquals(LifecycleReleaseOutcome.Released, registry.release(staleCapability))
        assertEquals(0, registry.retainedCount())
    }

    private fun valueEqualUnissued(
        issued: LifecycleOperationRegistry.Lease,
    ): LifecycleOperationRegistry.Lease =
        object : LifecycleOperationRegistry.Lease {
            override val lifecycle: SessionLifecycleRef = issued.lifecycle
            override val attempt: DeliveryAttemptRef? = issued.attempt
            override val operationId: UUID = issued.operationId
        }

    private fun expectViolation(block: () -> Unit): LifecycleReleaseViolation =
        try {
            block()
            throw AssertionError("expected lifecycle release violation")
        } catch (violation: LifecycleReleaseViolation) {
            violation
        }

    private fun expectPrimary(
        primary: Throwable,
        release: () -> Unit,
    ): Throwable = try {
        release()
        throw primary
    } catch (actual: Throwable) {
        actual
    }

    private fun assertReleaseViolation(
        violation: LifecycleReleaseViolation,
        outcome: LifecycleReleaseOutcome,
        site: LifecycleReleaseSite,
        capability: LifecycleOperationRegistry.Lease,
    ) {
        assertEquals(outcome, violation.outcome)
        assertEquals(site, violation.site)
        assertEquals(capability.lifecycle, violation.lifecycle)
        assertEquals(capability.attempt, violation.attempt)
        assertEquals(capability.operationId, violation.operationId)
    }

    private fun attempt(): DeliveryAttemptRef =
        DeliveryAttemptRef(
            ownerBareJid = OWNER,
            attemptId = DeliveryAttemptId.random(),
            nativeGeneration = NativeConnectionGeneration(1u),
        )

    private companion object {
        const val OWNER = "release@waddle.test"
    }
}
