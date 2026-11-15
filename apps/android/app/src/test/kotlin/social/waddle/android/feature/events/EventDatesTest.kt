package social.waddle.android.feature.events

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.client.ffi.WaddleCalendarDate

class EventDatesTest {
    @Test fun `dates can remain undecided`() {
        assertTrue(eventDatesValid("", ""))
        assertTrue(eventDatesValid("2027-02-12", ""))
        assertFalse(eventDatesValid("", "2027-02-13"))
    }

    @Test fun `all day end must be exclusive and later`() {
        assertTrue(eventDatesValid("2027-02-12", "2027-02-13"))
        assertFalse(eventDatesValid("2027-02-12", "2027-02-12"))
        assertFalse(eventDatesValid("2027-02-30", ""))
        assertFalse(eventDatesValid("2027-02-12", "2027-02-11"))
    }

    @Test fun `timed events require an offset and compare actual instants`() {
        assertTrue(eventDatesValid("2027-02-12T10:00:00Z", "2027-02-12T12:00:00+01:00"))
        assertFalse(eventDatesValid("2027-02-12T10:00:00Z", "2027-02-12T10:30:00+01:00"))
        assertFalse(eventDatesValid("2027-02-12T10:00:00", ""))
        assertFalse(eventDatesValid("2027-02-12", "2027-02-12T12:00:00Z"))
    }

    @Test fun `editing timed entries preserves the instant`() {
        val date = WaddleCalendarDate.DateTime(1802426400000)
        assertEquals(date, parseEventDate(eventDateText(date)))
    }
}
