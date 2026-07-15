package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ReconnectPolicyTest {
    @Test
    fun `exact delays with jitter pinned at lower bound`() {
        val policy = ReconnectPolicy(PinnedRandom(0.5))
        // min(2000 * 2^attempt, 60000) * 0.5
        assertEquals(1_000L, policy.delayMillisFor(0))
        assertEquals(2_000L, policy.delayMillisFor(1))
        assertEquals(4_000L, policy.delayMillisFor(2))
        assertEquals(8_000L, policy.delayMillisFor(3))
        assertEquals(16_000L, policy.delayMillisFor(4))
    }

    @Test
    fun `caps the base delay at sixty seconds`() {
        val policy = ReconnectPolicy(PinnedRandom(0.5))
        // Base would be 64s at attempt 5; capped at 60s before jitter.
        assertEquals(30_000L, policy.delayMillisFor(5))
        assertEquals(30_000L, policy.delayMillisFor(9))
    }

    @Test
    fun `jitter scales the base delay`() {
        val policy = ReconnectPolicy(PinnedRandom(0.75))
        assertEquals(1_500L, policy.delayMillisFor(0))
        assertEquals(45_000L, policy.delayMillisFor(9))
    }

    @Test
    fun `terminal after ten attempts`() {
        val policy = ReconnectPolicy(PinnedRandom(0.5))
        assertNull(policy.delayMillisFor(10))
        assertNull(policy.delayMillisFor(11))
    }

    @Test
    fun `default random stays within the jitter window`() {
        val policy = ReconnectPolicy()
        repeat(100) {
            val delay = policy.delayMillisFor(0) ?: error("attempt 0 must yield a delay")
            assertTrue("delay $delay below jitter floor", delay >= 1_000L)
            assertTrue("delay $delay above jitter ceiling", delay < 2_000L)
        }
    }
}
