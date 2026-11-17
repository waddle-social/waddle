package social.waddle.android.feature.events

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.client.ffi.WaddlePlanningKind

class PlanningValidationTest {
    @Test fun `London daylight saving gaps and duplicate times require a decision`() {
        assertTrue(runCatching { planningInstant("2027-03-28T01:30", "Europe/London") }.isFailure)
        assertTrue(runCatching { planningInstant("2027-10-31T01:30", "Europe/London") }.isFailure)
        assertEquals(1802426400000L, planningInstant("2027-02-12T10:00", "Europe/London"))
        assertNull(planningInstant("", "Europe/London"))
    }

    @Test fun `saved links reject executable schemes and credentials`() {
        assertNull(safePlanningUrl("javascript:alert(1)"))
        assertNull(safePlanningUrl("file:///etc/passwd"))
        assertNull(safePlanningUrl("https://user:password@example.com"))
        assertEquals("https://example.com/trip", safePlanningUrl("https://example.com/trip"))
    }

    @Test fun `calendar exports escape content and fold UTF8 safely`() {
        val event = newPlanningItem(WaddlePlanningKind.EVENT, "London, meetup; " + "🐧".repeat(30)).copy(
            startsAt = 1802426400000L, description = "A\nB",
        )
        val calendar = planningCalendar(listOf(event), "event-test@conference.example.com")
        assertTrue(calendar.contains("SUMMARY:London\\, meetup\\;"))
        assertTrue(calendar.contains("DESCRIPTION:A\\nB\r\n"))
        assertTrue(calendar.lines().all { it.toByteArray(Charsets.UTF_8).size <= 75 })
        assertFalse(calendar.contains("�"))
    }
}
