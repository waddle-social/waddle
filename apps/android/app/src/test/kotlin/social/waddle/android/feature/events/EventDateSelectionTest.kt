package social.waddle.android.feature.events

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import social.waddle.client.ffi.WaddleCalendarDate
import java.time.Instant
import java.time.LocalDate
import java.util.TimeZone

class EventDateSelectionTest {
    @Test fun `untouched known overlap instant is preserved but a new ambiguous selection is rejected`() {
        val original = Instant.parse("2027-10-31T01:30:37.123Z").toEpochMilli()
        val local = planningLocal(original, "Europe/London")
        assertNull(eventDateIssue(local, "", "Europe/London", originalStart = original))
        assertEquals(original, selectedPlanningInstant(local, "Europe/London", original))
        assertEquals(EventDateIssue.AMBIGUOUS_TIME, eventDateIssue(local, "", "Europe/London"))
        assertEquals(
            EventDateIssue.AMBIGUOUS_TIME,
            eventDateIssue(
                "2027-10-31T01:35", "", "Europe/London",
                originalStart = original,
            ),
        )
    }

    @Test fun `known overlap range compares instants even when local end appears earlier`() {
        val start = Instant.parse("2027-10-31T00:30:00Z").toEpochMilli()
        val end = Instant.parse("2027-10-31T01:15:00Z").toEpochMilli()
        val from = planningLocal(start, "Europe/London")
        val until = planningLocal(end, "Europe/London")
        assertEquals("2027-10-31T01:30", from)
        assertEquals("2027-10-31T01:15", until)
        assertNull(eventDateIssue(from, until, "Europe/London", originalStart = start, originalEnd = end))
        assertEquals(
            WaddleCalendarDate.DateTime(start), communityPickerValue(from, "Europe/London", false, false, start),
        )
        assertEquals(
            WaddleCalendarDate.DateTime(end), communityPickerValue(until, "Europe/London", false, true, end),
        )
    }

    @Test fun `calendar days stay on the selected day west of UTC`() {
        val previous = TimeZone.getDefault()
        try {
            TimeZone.setDefault(TimeZone.getTimeZone("Pacific/Honolulu"))
            val selected = LocalDate.of(2027, 2, 12)
            val millis = calendarPickerMillis(selected)
            assertEquals(Instant.parse("2027-02-12T00:00:00Z").toEpochMilli(), millis)
            assertEquals(selected, calendarPickerDate(millis))
        } finally {
            TimeZone.setDefault(previous)
        }
    }

    @Test fun `all day last day is inclusive in the editor and exclusive on the wire`() {
        val existing = WaddleCalendarDate.Date("2027-02-14")
        assertEquals("2027-02-13", communityPickerText(existing, "Europe/London", true))
        assertEquals(existing, communityPickerValue("2027-02-13", "Europe/London", true, true))
        assertNull(eventDateIssue("2027-02-12", "2027-02-12", "Europe/London", true))
        assertEquals(
            WaddleCalendarDate.Date("2027-02-13"),
            communityPickerValue("2027-02-12", "Europe/London", true, true),
        )
    }

    @Test fun `unchanged timed values preserve seconds and milliseconds in another zone`() {
        val existing = WaddleCalendarDate.DateTime(Instant.parse("2027-07-01T08:00:37.123Z").toEpochMilli())
        val local = communityPickerText(existing, "Europe/London", false)
        assertEquals("2027-07-01T09:00:37.123", local)
        assertEquals(existing, communityPickerValue(local, "Europe/London", false, false))
    }

    @Test fun `all day switch converts both fields and retains their days`() {
        val start = changeAllDay("2027-02-12T10:05:30", true, false)
        val end = changeAllDay("2027-02-13T15:09:30", true, true)
        assertEquals("2027-02-12", start)
        assertEquals("2027-02-13", end)
        assertEquals("2027-02-12T09:00", changeAllDay(start, false, false))
        assertEquals("2027-02-13T17:00", changeAllDay(end, false, true))
        assertEquals("", changeAllDay("", true, false))
    }

    @Test fun `optional dates can clear and invalid ranges explain the correction`() {
        assertNull(eventDateIssue("", "", "Europe/London"))
        assertNull(eventDateIssue("2027-02-12T10:00", "", "Europe/London"))
        assertEquals(EventDateIssue.START_REQUIRED, eventDateIssue("", "2027-02-12T11:00", "Europe/London"))
        assertEquals(
            EventDateIssue.END_BEFORE_START,
            eventDateIssue("2027-02-12T10:00", "2027-02-12T10:00", "Europe/London"),
        )
        assertEquals(
            EventDateIssue.LAST_DAY_BEFORE_START,
            eventDateIssue("2027-02-12", "2027-02-11", "Europe/London", true),
        )
        assertNull(communityPickerValue("", "Europe/London", false, false))
    }

    @Test fun `clock changes and invalid time zones have distinct actionable errors`() {
        assertEquals(EventDateIssue.SKIPPED_TIME, eventDateIssue("2027-03-28T01:30", "", "Europe/London"))
        assertEquals(EventDateIssue.AMBIGUOUS_TIME, eventDateIssue("2027-10-31T01:30", "", "Europe/London"))
        assertEquals(EventDateIssue.INVALID_ZONE, eventDateIssue("2027-02-12T10:00", "", "London"))
        assertNull(eventDateIssue("2027-03-28T02:30", "2027-03-28T03:30", "Europe/London"))
    }
}
