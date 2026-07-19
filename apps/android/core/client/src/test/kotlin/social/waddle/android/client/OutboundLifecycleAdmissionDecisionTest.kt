package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class OutboundLifecycleAdmissionDecisionTest {
    @Test
    fun `A bootstrapping denies offline live and terminal reservations until open`() {
        val lifecycle = SessionLifecycleRef.create("owner@waddle.test")
        val bootstrapping = OutboundLifecycleState.Bootstrapping(lifecycle)

        assertEquals(
            OutboundReservationClaim.LifecycleUnavailable,
            classifyOutboundReservation(bootstrapping, lifecycle.ownerBareJid),
        )
        assertNull(createAdmissionCandidate(bootstrapping, lifecycle.ownerBareJid, null, false))
        assertNull(createAdmissionCandidate(bootstrapping, lifecycle.ownerBareJid, null, true))
    }
}
