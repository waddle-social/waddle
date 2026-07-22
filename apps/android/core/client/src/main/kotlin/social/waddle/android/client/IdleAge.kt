package social.waddle.android.client

import java.time.OffsetDateTime

/**
 * XEP-0319 idle-age buckets, ported from the web's
 * `chat/src/presence/idle-duration.ts` (`formatIdle`) and
 * `ChatHeader.vue`: the idle age renders ONLY while the presence
 * `<show/>` is `away` or `xa`, at minute granularity, bucketed to
 * minutes → hours → days. The UI maps buckets to localized strings.
 */
sealed interface IdleAge {
    /** Idle for less than one minute ("idle <1m"). */
    data object UnderMinute : IdleAge

    data class Minutes(val minutes: Long) : IdleAge

    data class Hours(val hours: Long) : IdleAge

    data class Days(val days: Long) : IdleAge
}

private const val MINUTE_MS = 60_000L
private const val MINUTES_PER_HOUR = 60L
private const val HOURS_PER_DAY = 24L

/** Web `formatIdle` buckets; negative gaps clamp to [IdleAge.UnderMinute]. */
fun idleAgeOf(idleSinceRfc3339: String, nowMs: Long): IdleAge? {
    val sinceMs = runCatching {
        OffsetDateTime.parse(idleSinceRfc3339).toInstant().toEpochMilli()
    }.getOrNull() ?: return null
    val minutes = maxOf(0, nowMs - sinceMs) / MINUTE_MS
    return when {
        minutes < 1 -> IdleAge.UnderMinute
        minutes < MINUTES_PER_HOUR -> IdleAge.Minutes(minutes)
        minutes / MINUTES_PER_HOUR < HOURS_PER_DAY -> IdleAge.Hours(minutes / MINUTES_PER_HOUR)
        else -> IdleAge.Days(minutes / MINUTES_PER_HOUR / HOURS_PER_DAY)
    }
}

/** Web `ChatHeader.vue` gate: idle shows only for `away` / `xa`. */
fun presenceShowsIdle(show: String?): Boolean = show == "away" || show == "xa"
