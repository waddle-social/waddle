package social.waddle.android.feature.conversation

import android.app.Application
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import java.time.DayOfWeek
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.time.ZoneOffset
import java.util.Locale

/**
 * Times follow the device's locale and 12/24-hour setting (never a
 * static formatter), and day separators read Today, Yesterday, a
 * weekday within the week, else a short date.
 */
@RunWith(AndroidJUnit4::class)
@Config(sdk = [35], application = Application::class)
class ConversationTimeTest {
    private val today = LocalDate.of(2026, 10, 16) // a Friday
    private val morning = Instant.parse("2026-10-16T09:05:00Z")

    private fun clock(locale: Locale, use24Hour: Boolean, zone: ZoneId = ZoneOffset.UTC) =
        ConversationClock(locale = locale, use24Hour = use24Hour, zone = zone, today = today)

    @Test
    fun `today and yesterday are named`() {
        assertEquals(DayLabel.Today, dayLabelOf(today, today))
        assertEquals(DayLabel.Yesterday, dayLabelOf(today.minusDays(1), today))
    }

    @Test
    fun `two to six days ago read as the weekday`() {
        assertEquals(DayLabel.Weekday(DayOfWeek.WEDNESDAY), dayLabelOf(today.minusDays(2), today))
        assertEquals(DayLabel.Weekday(DayOfWeek.SATURDAY), dayLabelOf(today.minusDays(6), today))
    }

    @Test
    fun `a week or more ago reads as a date, with the year only when it differs`() {
        assertEquals(DayLabel.Date(today.minusDays(7), withYear = false), dayLabelOf(today.minusDays(7), today))
        val lastYear = LocalDate.of(2025, 12, 30)
        assertEquals(DayLabel.Date(lastYear, withYear = true), dayLabelOf(lastYear, today))
    }

    @Test
    fun `a clock-skewed future day is a date, never today`() {
        val tomorrow = today.plusDays(1)
        assertEquals(DayLabel.Date(tomorrow, withYear = false), dayLabelOf(tomorrow, today))
    }

    @Test
    fun `24-hour devices get a 24-hour time`() {
        assertEquals("09:05", clock(Locale.UK, use24Hour = true).time(morning))
        assertEquals("21:30", clock(Locale.US, use24Hour = true).time(Instant.parse("2026-10-16T21:30:00Z")))
    }

    @Test
    fun `12-hour devices get a 12-hour time, even in a 24-hour locale`() {
        val us = clock(Locale.US, use24Hour = false).time(Instant.parse("2026-10-16T21:30:00Z"))
        assertTrue(us, us.startsWith("9:30") && us.endsWith("PM"))
        val uk = clock(Locale.UK, use24Hour = false).time(morning)
        assertTrue(uk, uk.startsWith("9:05") && uk.lowercase(Locale.UK).endsWith("am"))
    }

    @Test
    fun `times are in the device zone`() {
        val tokyo = clock(Locale.UK, use24Hour = true, zone = ZoneId.of("Asia/Tokyo"))
        assertEquals("18:05", tokyo.time(morning))
        assertEquals(LocalDate.of(2026, 10, 16), tokyo.dayOf(morning))
    }

    @Test
    fun `day words follow the locale`() {
        val us = clock(Locale.US, use24Hour = false)
        assertEquals("Monday", us.words(DayLabel.Weekday(DayOfWeek.MONDAY)))
        assertEquals("Wed, Oct 7", us.words(DayLabel.Date(LocalDate.of(2026, 10, 7), withYear = false)))
        val uk = clock(Locale.UK, use24Hour = true)
        assertEquals("Wed, 7 Oct", uk.words(DayLabel.Date(LocalDate.of(2026, 10, 7), withYear = false)))
        val french = clock(Locale.FRANCE, use24Hour = true)
        assertEquals("lundi", french.words(DayLabel.Weekday(DayOfWeek.MONDAY)))
        assertNull(us.words(DayLabel.Today))
    }

    @Test
    fun `older dates carry their year`() {
        val label = DayLabel.Date(LocalDate.of(2025, 12, 30), withYear = true)
        assertEquals("Tue, Dec 30, 2025", clock(Locale.US, use24Hour = false).words(label))
    }

    @Test
    fun `a pinned today keeps the device's locale and hour setting`() {
        val pinned = clock(Locale.US, use24Hour = true).withToday(LocalDate.of(2026, 1, 2))
        assertEquals(LocalDate.of(2026, 1, 2), pinned.today)
        assertEquals("09:05", pinned.time(morning))
        assertEquals(DayLabel.Yesterday, pinned.labelOf(LocalDate.of(2026, 1, 1)))
    }
}
