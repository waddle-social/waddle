package social.waddle.android.feature.conversation

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.compose.LifecycleResumeEffect
import kotlinx.coroutines.delay
import social.waddle.android.R
import java.time.DayOfWeek
import java.time.Duration
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.time.ZonedDateTime
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
    /** The current instant, for "2h ago" labels; goldens pin it. */
    private val now: () -> Instant = Instant::now,
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
    fun withToday(day: LocalDate): ConversationClock = ConversationClock(locale, use24Hour, zone, day, bestPattern, now)

    /** How long before now [instant] was, bucketed for a compact label. */
    fun ageOf(instant: Instant): Age = ageBetween(instant, now(), this)

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

/** A compact "how long ago": now, minutes, hours, days, then the day itself. */
sealed interface Age {
    data object Now : Age

    data class Minutes(val minutes: Long) : Age

    data class Hours(val hours: Long) : Age

    data class Days(val days: Long) : Age

    /** A week or more ago: the date reads better than a count. */
    data class On(val label: DayLabel) : Age
}

/** [Age] buckets (web relative-time parity); a future [instant] (clock skew) is [Age.Now]. */
fun ageBetween(instant: Instant, now: Instant, clock: ConversationClock): Age {
    val minutes = Duration.between(instant, now).toMinutes()
    val hours = minutes / MINUTES_PER_HOUR
    val days = hours / HOURS_PER_DAY
    return when {
        minutes < 1 -> Age.Now
        hours < 1 -> Age.Minutes(minutes)
        days < 1 -> Age.Hours(hours)
        days < DAYS_IN_WEEK -> Age.Days(days)
        else -> Age.On(clock.labelOf(clock.dayOf(instant)))
    }
}

/** The compact words for an [Age]: "now", "5m", "2h", "3d" or a date. */
@Composable
fun ConversationClock.ageText(age: Age): String = when (age) {
    Age.Now -> stringResource(R.string.age_now)
    is Age.Minutes -> stringResource(R.string.age_minutes, age.minutes)
    is Age.Hours -> stringResource(R.string.age_hours, age.hours)
    is Age.Days -> stringResource(R.string.age_days, age.days)
    is Age.On -> dayText(age.label)
}

/** An [Age] as a screen reader says it: "just now", "5 minutes ago", "on Fri 9 Oct". */
@Composable
fun ConversationClock.spokenAge(age: Age): String = when (age) {
    Age.Now -> stringResource(R.string.age_spoken_now)
    is Age.Minutes -> pluralStringResource(R.plurals.age_spoken_minutes, age.minutes.toInt(), age.minutes)
    is Age.Hours -> pluralStringResource(R.plurals.age_spoken_hours, age.hours.toInt(), age.hours)
    is Age.Days -> pluralStringResource(R.plurals.age_spoken_days, age.days.toInt(), age.days)
    is Age.On -> stringResource(R.string.age_spoken_on, dayText(age.label))
}

/**
 * A count that moves on once a minute while it is composed, for labels
 * that say how long ago something was. Read it to recompute those ages.
 */
@Composable
fun rememberMinuteTick(): Int {
    var minutes by remember { mutableIntStateOf(0) }
    LaunchedEffect(Unit) {
        while (true) {
            delay(MINUTE_MILLIS)
            minutes += 1
        }
    }
    return minutes
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
    // A screen left open across midnight must roll Today over too, not
    // only on the next resume.
    var midnights by remember { mutableIntStateOf(0) }
    LaunchedEffect(resumes, midnights) {
        delay(millisUntilNextLocalMidnight())
        midnights += 1
    }
    return remember(locale, resumes, midnights) { deviceClock(context, locale) }
}

private fun millisUntilNextLocalMidnight(): Long {
    val zone = ZoneId.systemDefault()
    val now = ZonedDateTime.now(zone)
    val next = now.toLocalDate().plusDays(1).atStartOfDay(zone)
    return Duration.between(now, next).toMillis() + MIDNIGHT_SLACK_MILLIS
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
private const val MINUTES_PER_HOUR = 60L
private const val HOURS_PER_DAY = 24L
private const val MINUTE_MILLIS = 60_000L

/** Lands just past midnight, so LocalDate.now() is already the new day. */
private const val MIDNIGHT_SLACK_MILLIS = 50L
