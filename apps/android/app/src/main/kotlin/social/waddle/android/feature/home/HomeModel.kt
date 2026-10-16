package social.waddle.android.feature.home

import social.waddle.android.feature.rooms.RoomActivity

/** Web `heroTimeOfDayFor`: which greeting and which quiet line. */
enum class TimeOfDay {
    /** 05:00–10:59 */
    MORNING,

    /** 11:00–16:59 */
    DAY,

    /** 17:00–21:59 */
    EVENING,

    /** 22:00–04:59 */
    NIGHT,
    ;

    companion object {
        fun of(hour: Int): TimeOfDay = when (hour) {
            in MORNING_START until DAY_START -> MORNING
            in DAY_START until EVENING_START -> DAY
            in EVENING_START until NIGHT_START -> EVENING
            else -> NIGHT
        }

        private const val MORNING_START = 5
        private const val DAY_START = 11
        private const val EVENING_START = 17
        private const val NIGHT_START = 22
    }
}

/**
 * The counts behind the hero summary line (web `HeroSummary`). Unread
 * counts rooms that are not muted plus direct messages; mentions are
 * live-only, so the line never claims more than this session saw.
 */
data class HomeSummary(
    val mentions: Int = 0,
    val unread: Int = 0,
    val threadReplies: Int = 0,
    val activeCalls: Int = 0,
    val around: Int = 0,
)

/** One "**2** mentions" segment of the summary line. */
enum class SummaryPart { MENTIONS, UNREAD, THREAD_REPLIES, ACTIVE_CALLS, AROUND }

/** Web `heroSummaryPartsFor`: the segments worth saying, zeroes dropped, in this order. */
fun summaryPartsOf(summary: HomeSummary): List<Pair<SummaryPart, Int>> = listOf(
    SummaryPart.MENTIONS to summary.mentions,
    SummaryPart.UNREAD to summary.unread,
    SummaryPart.THREAD_REPLIES to summary.threadReplies,
    SummaryPart.ACTIVE_CALLS to summary.activeCalls,
    SummaryPart.AROUND to summary.around,
).filter { (_, count) -> count > 0 }

fun summaryOf(rooms: List<RoomActivity>, dmUnread: Int, around: Int): HomeSummary = HomeSummary(
    mentions = rooms.sumOf { it.mentions },
    unread = rooms.sumOf { it.loudUnread } + dmUnread,
    threadReplies = rooms.sumOf { it.threadUnread },
    activeCalls = rooms.count { it.live },
    around = around,
)

/** The hero's one primary action (web `heroCtaLabel`). */
sealed interface HeroAction {
    /** A live huddle this device can join: ember. */
    data class Join(val room: RoomActivity) : HeroAction

    /** This device is in that huddle: back into the room. */
    data class Return(val room: RoomActivity) : HeroAction

    /** Nothing live: the busiest room. */
    data class JumpInto(val room: RoomActivity) : HeroAction

    /** Nothing at all: go and look around. */
    data object Browse : HeroAction
}

fun heroActionOf(live: List<RoomActivity>, busiest: List<RoomActivity>): HeroAction {
    val here = live.firstOrNull { it.youAreInHuddle }
    val joinable = live.firstOrNull { it.canJoin }
    return when {
        here != null -> HeroAction.Return(here)
        joinable != null -> HeroAction.Join(joinable)
        busiest.isNotEmpty() -> HeroAction.JumpInto(busiest.first())
        else -> HeroAction.Browse
    }
}

/**
 * Web `compareChannelActivityPriority`: mentions, then unread plus
 * thread replies, then the newest message, then name.
 */
val ActivityPriority: Comparator<RoomActivity> =
    compareByDescending<RoomActivity> { it.mentions }
        .thenByDescending { it.loudUnread + it.threadUnread }
        .thenByDescending { it.lastUpdated ?: Long.MIN_VALUE }
        .thenBy(String.CASE_INSENSITIVE_ORDER) { it.name }
        .thenBy { it.roomJid }

/** What the Home body lists below the hero. */
data class HomeSections(
    /** Live huddles, biggest first: the ember cards. */
    val live: List<RoomActivity>,
    /** Busiest rooms after the live ones, so "Happening now" holds at most [HAPPENING_NOW_LIMIT]. */
    val busiest: List<RoomActivity>,
    /** Rooms where someone expects you: mentions or thread replies. */
    val needsYou: List<RoomActivity>,
)

/**
 * Web `HomeDashboard`: live huddles first, then the busiest rooms by
 * activity, at most three cards; rooms that need you go below instead
 * and are not repeated.
 */
fun homeSectionsOf(rooms: List<RoomActivity>): HomeSections {
    val live = rooms.filter { it.live }
        .sortedWith(compareByDescending<RoomActivity> { it.huddle.size }.then(ActivityPriority))
        .take(HAPPENING_NOW_LIMIT)
    val active = rooms.filter { it.hasActivity }.sortedWith(ActivityPriority)
    val needsYou = active.filter { it.needsYou }
    val busiest = active.filter { !it.needsYou && !it.live }.take(HAPPENING_NOW_LIMIT - live.size)
    return HomeSections(live = live, busiest = busiest, needsYou = needsYou)
}

/** Nothing to report: the mascot and a quiet line instead of numbers. */
fun isQuiet(summary: HomeSummary): Boolean = summaryPartsOf(summary).isEmpty()

/** "mara, sol +1" (web `mucCallParticipantPreview`, two names then a count). */
fun participantPreviewOf(names: List<String>, maxVisible: Int = 2): String {
    val visible = names.map { it.trim() }.filter { it.isNotEmpty() }
    if (visible.size <= maxVisible) return visible.joinToString(", ")
    return visible.take(maxVisible).joinToString(", ") + " +" + (visible.size - maxVisible)
}

const val HAPPENING_NOW_LIMIT = 3
