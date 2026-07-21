package social.waddle.android.client

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.DeliveryAttemptId
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.NativeConnectionGeneration
import java.util.UUID

class LifecycleOperationRegistryTest {
    @Test
    fun `programmer cleanup failure is not converted into retryable durability`() = runTest {
        try {
            durableCleanupBoundary(DurableCleanupOperation.JOURNAL_FENCE) {
                error("programmer failure")
            }
            throw AssertionError("expected programmer failure to propagate")
        } catch (expected: IllegalStateException) {
            assertEquals("programmer failure", expected.message)
        }
    }

    @Test
    fun `only the exact issued lease can release an active operation`() {
        val lifecycle = SessionLifecycleRef.create("owner@waddle.test")
        val registry = LifecycleOperationRegistry(lifecycle)
        val lease = checkNotNull(registry.issue(null))

        assertTrue(registry.owns(lease))
        assertEquals(LifecycleReleaseOutcome.Released, registry.release(lease))
        assertEquals(LifecycleReleaseOutcome.AlreadyReleased, registry.release(lease))
        assertEquals(0, registry.retainedCount())
    }

    @Test
    fun `operation identity cannot create a second active lease or resurrect history`() {
        val lifecycle = SessionLifecycleRef.create("owner@waddle.test")
        val registry = LifecycleOperationRegistry(lifecycle)
        val operation = UUID.randomUUID()
        val lease = checkNotNull(registry.issue(null, operation))

        assertNull(registry.issue(null, operation))
        assertEquals(LifecycleReleaseOutcome.Released, registry.release(lease))
        val replacement = checkNotNull(registry.issue(null, operation))
        assertTrue(registry.owns(replacement))
        assertEquals(LifecycleReleaseOutcome.AlreadyReleased, registry.release(lease))
        assertEquals(LifecycleReleaseOutcome.Released, registry.release(replacement))
        assertEquals(0, registry.retainedCount())
    }

    @Test
    fun `same operation identity with a different attempt is rejected while active`() {
        val lifecycle = SessionLifecycleRef.create("owner@waddle.test")
        val registry = LifecycleOperationRegistry(lifecycle)
        val operation = UUID.randomUUID()
        val issued = checkNotNull(registry.issue(attempt("owner@waddle.test", 1u), operation))

        assertNull(registry.issue(attempt("owner@waddle.test", 2u), operation))
        assertEquals(LifecycleReleaseOutcome.Released, registry.release(issued))
        assertEquals(0, registry.retainedCount())
    }

    @Test
    fun `wrong lifecycle attempt and foreign lifecycle registry are rejected`() {
        val lifecycle = SessionLifecycleRef.create("owner@waddle.test")
        val registry = LifecycleOperationRegistry(lifecycle)
        val issued = checkNotNull(registry.issue(attempt("owner@waddle.test", 1u)))
        val otherLifecycle = SessionLifecycleRef.create("other@waddle.test")
        val foreign = LifecycleOperationRegistry(otherLifecycle)

        assertNull(registry.issue(attempt("other@waddle.test", 2u)))
        assertEquals(LifecycleReleaseOutcome.NotOwned, foreign.release(issued))
        assertEquals(LifecycleReleaseOutcome.Released, registry.release(issued))
    }

    @Test
    fun `foreign registry cannot observe or release an issued lease`() {
        val lifecycle = SessionLifecycleRef.create("owner@waddle.test")
        val issuer = LifecycleOperationRegistry(lifecycle)
        val foreign = LifecycleOperationRegistry(lifecycle)
        val lease = checkNotNull(issuer.issue(null))

        assertEquals(LifecycleReleaseOutcome.NotOwned, foreign.release(lease))
        assertTrue(issuer.owns(lease))
        assertEquals(LifecycleReleaseOutcome.Released, issuer.release(lease))
    }

    @Test
    fun `value equal never issued lease is rejected without affecting exact authority`() {
        val lifecycle = SessionLifecycleRef.create("owner@waddle.test")
        val registry = LifecycleOperationRegistry(lifecycle)
        val issued = checkNotNull(registry.issue(attempt("owner@waddle.test", 1u)))
        val forged = object : LifecycleOperationRegistry.Lease {
            override val lifecycle = issued.lifecycle
            override val attempt = issued.attempt
            override val operationId = issued.operationId
        }

        assertEquals(LifecycleReleaseOutcome.NotOwned, registry.release(forged))
        assertTrue(registry.owns(issued))
        assertEquals(LifecycleReleaseOutcome.Released, registry.release(issued))
    }

    @Test
    fun `released exact lease cannot be reused and registry retains no history`() {
        val lifecycle = SessionLifecycleRef.create("owner@waddle.test")
        val registry = LifecycleOperationRegistry(lifecycle)
        val operation = UUID.randomUUID()
        val issued = checkNotNull(registry.issue(null, operation))

        assertEquals(LifecycleReleaseOutcome.Released, registry.release(issued))
        assertFalse(registry.owns(issued))
        val replacement = checkNotNull(registry.issue(null, operation))
        assertEquals(LifecycleReleaseOutcome.AlreadyReleased, registry.release(issued))
        assertTrue(registry.owns(replacement))
        assertEquals(LifecycleReleaseOutcome.Released, registry.release(replacement))
        assertEquals(0, registry.retainedCount())
    }

    @Test
    fun `repeated issue and release returns active registry storage to zero`() {
        val lifecycle = SessionLifecycleRef.create("owner@waddle.test")
        val registry = LifecycleOperationRegistry(lifecycle)

        repeat(128) { index ->
            val lease = checkNotNull(registry.issue(null, UUID.randomUUID()))
            assertEquals(LifecycleReleaseOutcome.Released, registry.release(lease))
            assertEquals("cycle $index", 0, registry.retainedCount())
        }
    }

    @Test
    fun `G retained lease completes after a fence closes new admissions`() {
        val lifecycle = SessionLifecycleRef.create("owner@waddle.test")
        val registry = LifecycleOperationRegistry(lifecycle)
        val retained = checkNotNull(registry.issue(null))

        registry.closeAdmissions()

        assertNull(registry.issue(null))
        assertEquals(LifecycleReleaseOutcome.Released, registry.release(retained))
        assertEquals(0, registry.retainedCount())
    }

    private fun attempt(ownerBareJid: String, generation: ULong): DeliveryAttemptRef =
        DeliveryAttemptRef(
            ownerBareJid = ownerBareJid,
            attemptId = DeliveryAttemptId.random(),
            nativeGeneration = NativeConnectionGeneration(generation),
        )
}
