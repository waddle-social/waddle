package social.waddle.android.feature.conversation

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.compose.LifecycleResumeEffect
import social.waddle.android.R
import java.time.DayOfWeek
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.TextStyle
import java.util.Locale
import android.text.format.DateFormat as AndroidDateFormat

/** What a day separator says, before it is put into words. */
sealed interface DayLabel {
    data object Today : DayLabel

    data object Yesterday : DayLabel

    /** Two to six days ago: the weekday reads faster than a date. */
    data class Weekday(val day: DayOfWeek) : DayLabel

    /** Older (or clock-skewed future) days; the year only when it is not this one. */
    data class Date(val date: LocalDate, val withYear: Boolean) : DayLabel
}

/** "Today", "Yesterday", a weekday within the last week, else a short date (web/iOS parity). */
fun dayLabelOf(day: LocalDate, today: LocalDate): DayLabel = when {
    day == today -> DayLabel.Today
    day == today.minusDays(1) -> DayLabel.Yesterday
    day.isBefore(today) && day.isAfter(today.minusDays(DAYS_IN_WEEK)) -> DayLabel.Weekday(day.dayOfWeek)
    else -> DayLabel.Date(day, withYear = day.year != today.year)
}

/**
 * How this device writes times and dates right now: the current locale,
 * the system 12/24-hour setting, the time zone and today's date. Built
 * per composition from the live configuration (never a static formatter
 * that caches the JVM default), re-read on every resume, and injected in
 * tests and goldens through [LocalConversationClock].
 */
@Immutable
class ConversationClock(
    val locale: Locale,
    val use24Hour: Boolean,
    val zone: ZoneId,
    val today: LocalDate,
    /** Locale skeleton → pattern; Android's ICU best pattern in production. */
    private val bestPattern: (Locale, String) -> String = ::androidBestPattern,
) {
    private val timeFormatter = DateTimeFormatter.ofPattern(
        bestPattern(locale, if (use24Hour) SKELETON_24H else SKELETON_12H),
        locale,
    )
    private val dateFormatter = DateTimeFormatter.ofPattern(bestPattern(locale, SKELETON_DATE), locale)
    private val dateYearFormatter = DateTimeFormatter.ofPattern(bestPattern(locale, SKELETON_DATE_YEAR), locale)

    /** A message time: "09:05" or "9:05 AM", as the device says it. */
    fun time(instant: Instant): String = timeFormatter.format(instant.atZone(zone))

    /** The same device on another day (screenshots pin "today"). */
    fun withToday(day: LocalDate): ConversationClock = ConversationClock(locale, use24Hour, zone, day, bestPattern)

    fun dayOf(instant: Instant): LocalDate = instant.atZone(zone).toLocalDate()

    fun labelOf(day: LocalDate): DayLabel = dayLabelOf(day, today)

    /** The weekday or date words of [label]; "Today"/"Yesterday" come from resources. */
    fun words(label: DayLabel): String? = when (label) {
        DayLabel.Today, DayLabel.Yesterday -> null
        is DayLabel.Weekday -> label.day.getDisplayName(TextStyle.FULL, locale)
        is DayLabel.Date -> (if (label.withYear) dateYearFormatter else dateFormatter).format(label.date)
    }

    /** "Today", a weekday or a date, plus the time: for previews outside the timeline. */
    @Composable
    fun stamp(instant: Instant): String {
        val day = dayOf(instant)
        return if (day == today) time(instant) else "${dayText(labelOf(day))} · ${time(instant)}"
    }

    override fun equals(other: Any?): Boolean = other is ConversationClock &&
        other.locale == locale &&
        other.use24Hour == use24Hour &&
        other.zone == zone &&
        other.today == today

    override fun hashCode(): Int = listOf(locale, use24Hour, zone, today).hashCode()
}

/** The words for a day separator. */
@Composable
fun ConversationClock.dayText(label: DayLabel): String = when (label) {
    DayLabel.Today -> stringResource(R.string.timeline_today)
    DayLabel.Yesterday -> stringResource(R.string.timeline_yesterday)
    else -> words(label).orEmpty()
}

/** Overrides the device clock (tests, goldens). `null` reads the device. */
val LocalConversationClock = staticCompositionLocalOf<ConversationClock?> { null }

/**
 * The provided clock, or the device's: locale from the live
 * configuration, the 12/24-hour setting and today re-read on resume.
 */
@Composable
fun rememberConversationClock(): ConversationClock {
    LocalConversationClock.current?.let { return it }
    val context = LocalContext.current
    val locale = LocalConfiguration.current.locales[0]
    var resumes by remember { mutableIntStateOf(0) }
    LifecycleResumeEffect(Unit) {
        resumes += 1
        onPauseOrDispose { }
    }
    return remember(locale, resumes) { deviceClock(context, locale) }
}

private fun deviceClock(context: Context, locale: Locale): ConversationClock {
    val zone = ZoneId.systemDefault()
    return ConversationClock(
        locale = locale,
        use24Hour = AndroidDateFormat.is24HourFormat(context),
        zone = zone,
        today = LocalDate.now(zone),
    )
}

private fun androidBestPattern(locale: Locale, skeleton: String): String =
    AndroidDateFormat.getBestDateTimePattern(locale, skeleton)

private const val SKELETON_24H = "Hm"
private const val SKELETON_12H = "hm"
private const val SKELETON_DATE = "EEEMMMd"
private const val SKELETON_DATE_YEAR = "EEEMMMdy"
private const val DAYS_IN_WEEK = 7L
