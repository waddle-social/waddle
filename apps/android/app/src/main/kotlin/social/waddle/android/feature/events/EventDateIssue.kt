package social.waddle.android.feature.events

import androidx.annotation.StringRes
import social.waddle.android.R
import social.waddle.client.ffi.WaddleCalendarDate
import java.time.Instant
import java.time.LocalDate
import java.time.LocalDateTime
import java.time.LocalTime
import java.time.ZoneId

internal enum class EventDateIssue(@param:StringRes val message: Int) {
    INVALID_ZONE(R.string.events_invalid_zone),
    INVALID_DATE(R.string.events_invalid_date),
    SKIPPED_TIME(R.string.events_skipped_time),
    AMBIGUOUS_TIME(R.string.events_ambiguous_time),
    START_REQUIRED(R.string.events_start_required),
    END_BEFORE_START(R.string.events_end_after_start),
    LAST_DAY_BEFORE_START(R.string.events_last_day_after_start),
}

/** Validate wall-clock selections before resolving them to the chosen zone. */
internal fun eventDateIssue(
    start: String,
    end: String,
    timezone: String,
    allDay: Boolean = false,
    originalStart: Long? = null,
    originalEnd: Long? = null,
): EventDateIssue? {
    if (start.isEmpty() && end.isNotEmpty()) return EventDateIssue.START_REQUIRED
    if (allDay) return allDayDateIssue(start, end)
    val zone = runCatching { ZoneId.of(timezone) }.getOrNull() ?: return EventDateIssue.INVALID_ZONE
    val from = eventWallTime(start, zone, originalStart)
    val until = eventWallTime(end, zone, originalEnd)
    if (from.second != null) return from.second
    if (until.second != null) return until.second
    return if (from.first != null && until.first != null && until.first!! <= from.first!!) {
        EventDateIssue.END_BEFORE_START
    } else {
        null
    }
}

private fun eventWallTime(value: String, zone: ZoneId, original: Long?): Pair<Instant?, EventDateIssue?> {
    if (value.isEmpty()) return null to null
    if (original != null && value == planningLocal(original, zone.id)) return Instant.ofEpochMilli(original) to null
    val local = runCatching { LocalDateTime.parse(value) }.getOrNull() ?: return null to EventDateIssue.INVALID_DATE
    val offsets = zone.rules.getValidOffsets(local)
    if (offsets.isEmpty()) return null to EventDateIssue.SKIPPED_TIME
    if (offsets.size != 1) return null to EventDateIssue.AMBIGUOUS_TIME
    return local.toInstant(offsets.single()) to null
}

/** A known instant stays exact until its local date, time or zone changes. */
internal fun selectedPlanningInstant(value: String, timezone: String, original: Long?): Long? =
    if (original != null && value == planningLocal(original, timezone)) original else planningInstant(value, timezone)

private fun allDayDateIssue(start: String, end: String): EventDateIssue? = runCatching {
    val from = start.takeIf { it.isNotEmpty() }?.let(LocalDate::parse)
    val until = end.takeIf { it.isNotEmpty() }?.let(LocalDate::parse)
    if (from != null && until != null && until < from) EventDateIssue.LAST_DAY_BEFORE_START else null
}.getOrDefault(EventDateIssue.INVALID_DATE)

/** All-day end dates are inclusive in the editor and exclusive in iCalendar. */
internal fun communityPickerValue(
    value: String,
    timezone: String,
    allDay: Boolean,
    end: Boolean,
    original: Long? = null,
): WaddleCalendarDate? {
    if (value.isEmpty()) return null
    return if (allDay) {
        val date = LocalDate.parse(value)
        WaddleCalendarDate.Date((if (end) date.plusDays(1) else date).toString())
    } else {
        WaddleCalendarDate.DateTime(selectedPlanningInstant(value, timezone, original)!!)
    }
}

internal fun communityPickerText(value: WaddleCalendarDate?, timezone: String, end: Boolean): String = when (value) {
    is WaddleCalendarDate.Date -> (LocalDate.parse(value.date).let { if (end) it.minusDays(1) else it }).toString()
    is WaddleCalendarDate.DateTime -> planningLocal(value.timestampMs, timezone)
    null -> ""
}

/** Convert both fields visibly when the user changes the all-day setting. */
internal fun changeAllDay(value: String, allDay: Boolean, end: Boolean): String {
    if (value.isEmpty()) return ""
    val date = LocalDate.parse(value.take(10))
    return if (allDay) date.toString() else LocalDateTime.of(date, LocalTime.of(if (end) 17 else 9, 0)).toString()
}
