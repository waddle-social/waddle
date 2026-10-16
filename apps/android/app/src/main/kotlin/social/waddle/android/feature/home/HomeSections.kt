package social.waddle.android.feature.home

import android.text.format.DateFormat
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Call
import androidx.compose.material.icons.outlined.Videocam
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import social.waddle.android.R
import social.waddle.android.feature.people.PersonRow
import social.waddle.android.feature.rooms.HuddleStack
import social.waddle.android.feature.rooms.RoomActivity
import social.waddle.android.feature.rooms.RoomTitle
import social.waddle.android.feature.rooms.roomBodyOf
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.huddle.AvatarSize
import social.waddle.android.ui.huddle.CountBadge
import social.waddle.android.ui.huddle.CountTone
import social.waddle.android.ui.huddle.HuddleAvatar
import social.waddle.android.ui.huddle.HuddleCard
import social.waddle.android.ui.huddle.Kicker
import social.waddle.android.ui.huddle.LiveDot
import social.waddle.android.ui.huddle.PillButton
import social.waddle.android.ui.huddle.PillTone
import social.waddle.android.ui.huddle.SkeletonBlock
import social.waddle.android.ui.listTimeOf
import java.time.Instant
import java.time.ZonedDateTime
import java.time.format.DateTimeFormatter

/**
 * Who is here, as faces (phone artboard): 44dp avatars with presence
 * and huddle rings, huddles first, a short name under each, and "+N"
 * for everyone who does not fit. Taps open the DM; "+N" opens People.
 */
@Composable
fun WhoIsHereStrip(
    people: List<PersonRow>,
    onOpen: (PersonRow) -> Unit,
    onMore: () -> Unit,
    modifier: Modifier = Modifier,
) {
    BoxWithConstraints(modifier = modifier.fillMaxWidth()) {
        val slots = ((maxWidth + Waddle.space.xs) / (SLOT_WIDTH + Waddle.space.xs)).toInt().coerceAtLeast(1)
        val overflow = people.size > slots
        val shown = if (overflow) people.take(slots - 1) else people
        Row(horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs)) {
            shown.forEach { person -> StripSlot(person = person, onClick = { onOpen(person) }) }
            if (overflow) MoreSlot(count = people.size - shown.size, onClick = onMore)
        }
    }
}

@Composable
private fun StripSlot(person: PersonRow, onClick: () -> Unit) {
    val message = stringResource(R.string.people_message, person.name)
    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
        modifier = Modifier
            .width(SLOT_WIDTH)
            .clip(RoundedCornerShape(Waddle.radius.control))
            .clickable(onClickLabel = message, role = Role.Button, onClick = onClick)
            .padding(vertical = Waddle.space.xxs),
    ) {
        HuddleAvatar(
            jid = person.jid,
            name = person.name,
            size = AvatarSize.LG,
            presence = person.show,
            inHuddle = person.inHuddle,
            speaking = person.speaking,
        )
        Text(
            text = person.name,
            style = MaterialTheme.typography.labelSmall,
            color = if (person.inHuddle || person.speaking) Waddle.colors.fg else Waddle.colors.fgMuted,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            textAlign = TextAlign.Center,
        )
    }
}

@Composable
private fun MoreSlot(count: Int, onClick: () -> Unit) {
    val description = pluralStringResource(R.plurals.home_strip_more_description, count, count)
    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        modifier = Modifier
            .width(SLOT_WIDTH)
            .clip(RoundedCornerShape(Waddle.radius.control))
            .clickable(role = Role.Button, onClick = onClick)
            .semantics { contentDescription = description }
            .padding(vertical = Waddle.space.xxs),
    ) {
        Box(
            contentAlignment = Alignment.Center,
            modifier = Modifier
                .clearAndSetSemantics { }
                .size(AvatarSize.LG.dp)
                .background(Waddle.colors.surface2, CircleShape)
                .border(1.dp, Waddle.colors.border, CircleShape),
        ) {
            Text(
                text = stringResource(R.string.home_strip_more, count),
                style = Waddle.type.time,
                color = Waddle.colors.fg,
                maxLines = 1,
            )
        }
    }
}

/** What the hero card says, already resolved: its strings come from [HomeHero]. */
data class HeroModel(
    val now: ZonedDateTime,
    val name: String?,
    val around: Int,
    val inHuddle: Int,
    val anyLive: Boolean,
    val summary: HomeSummary,
    val action: HeroAction,
    val loading: Boolean,
)

/**
 * The greeting card (web `HomeDashboard` hero): a mono kicker with the
 * date and who is around (zero segments dropped, an ember dot when
 * anything is live), the greeting by time of day, then either what is
 * waiting in bold counts or a quiet line with the mascot, and one action.
 */
@Composable
fun HomeHero(model: HeroModel, onAction: (HeroAction) -> Unit, modifier: Modifier = Modifier) {
    val quiet = !model.loading && isQuiet(model.summary)
    HuddleCard(modifier = modifier.fillMaxWidth(), contentPadding = PaddingValues(Waddle.space.lg)) {
        Row(horizontalArrangement = Arrangement.spacedBy(Waddle.space.md)) {
            Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(Waddle.space.sm)) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
                ) {
                    if (model.anyLive) LiveDot()
                    // Not the one-line Kicker: at large text sizes the
                    // counts wrap instead of being cut off.
                    Text(
                        text = heroKickerOf(model).uppercase(LocalConfiguration.current.locales[0]),
                        style = Waddle.type.kicker,
                        color = Waddle.colors.fgMuted,
                    )
                }
                Text(
                    text = greetingOf(model.now, model.name),
                    style = Waddle.type.headline,
                    color = Waddle.colors.fg,
                    modifier = Modifier.semantics { heading() },
                )
            }
            if (quiet) {
                Image(
                    painter = painterResource(R.drawable.waddle_mark),
                    contentDescription = null,
                    modifier = Modifier.size(MASCOT),
                )
            }
        }
        Spacer(Modifier.size(Waddle.space.sm))
        if (model.loading) {
            SkeletonBlock(Modifier.fillMaxWidth(SKELETON_LINE_LONG))
            SkeletonBlock(Modifier.fillMaxWidth(SKELETON_LINE_SHORT))
        } else {
            Text(
                text = if (quiet) AnnotatedString(quietLineOf(model.now)) else summaryTextOf(model.summary),
                style = Waddle.type.body,
                color = Waddle.colors.fgMuted,
            )
            Spacer(Modifier.size(Waddle.space.md))
            HeroButton(action = model.action, onAction = onAction)
        }
    }
}

@Composable
private fun HeroButton(action: HeroAction, onAction: (HeroAction) -> Unit) {
    when (action) {
        is HeroAction.Join -> PillButton(
            text = stringResource(R.string.home_cta_join, action.room.name),
            onClick = { onAction(action) },
            tone = PillTone.LIVE,
            icon = if (action.room.video) Icons.Outlined.Videocam else Icons.Outlined.Call,
        )
        is HeroAction.Return -> PillButton(
            text = stringResource(R.string.home_cta_return, action.room.name),
            onClick = { onAction(action) },
            tone = PillTone.LIVE,
            icon = if (action.room.video) Icons.Outlined.Videocam else Icons.Outlined.Call,
        )
        is HeroAction.JumpInto -> PillButton(
            text = stringResource(R.string.home_cta_jump, action.room.name),
            onClick = { onAction(action) },
        )
        HeroAction.Browse -> PillButton(
            text = stringResource(R.string.home_cta_browse),
            onClick = { onAction(action) },
            tone = PillTone.QUIET,
        )
    }
}

/** "Fri 16 Oct · 12 around · 3 in a huddle": the date in the reader's locale, zeroes dropped. */
@Composable
private fun heroKickerOf(model: HeroModel): String {
    val locale = LocalConfiguration.current.locales[0]
    val pattern = DateFormat.getBestDateTimePattern(locale, DATE_SKELETON)
    val parts = buildList {
        // Web parity ("Fri 16 Oct"): the kicker's dots already separate.
        add(DateTimeFormatter.ofPattern(pattern, locale).format(model.now).replace(",", ""))
        if (model.around > 0) add(stringResource(R.string.home_kicker_around, model.around))
        if (model.inHuddle > 0) add(stringResource(R.string.home_kicker_in_huddle, model.inHuddle))
    }
    // Each segment stays whole when large text wraps the line.
    return parts.joinToString(NBSP + "· ") { it.replace(' ', NBSP) }
}

/** "Good afternoon, David." by the hour of [now] (web `heroGreetingFor`). */
@Composable
fun greetingOf(now: ZonedDateTime, name: String?): String {
    val base = stringResource(
        when (TimeOfDay.of(now.hour)) {
            TimeOfDay.MORNING -> R.string.home_greeting_morning
            TimeOfDay.DAY -> R.string.home_greeting_day
            TimeOfDay.EVENING -> R.string.home_greeting_evening
            TimeOfDay.NIGHT -> R.string.home_greeting_night
        },
    )
    val trimmed = name?.trim().orEmpty()
    return if (trimmed.isEmpty()) {
        stringResource(R.string.home_greeting_plain, base)
    } else {
        stringResource(R.string.home_greeting_named, base, trimmed)
    }
}

/** The web's quiet line for the hour of [now] (web `heroQuietMessageFor`). */
@Composable
fun quietLineOf(now: ZonedDateTime): String = stringResource(
    when (TimeOfDay.of(now.hour)) {
        TimeOfDay.MORNING -> R.string.home_quiet_morning
        TimeOfDay.DAY -> R.string.home_quiet_day
        TimeOfDay.EVENING -> R.string.home_quiet_evening
        TimeOfDay.NIGHT -> R.string.home_quiet_night
    },
)

/** "**2** mentions · **5** unread messages": counts bold in the foreground, labels muted. */
@Composable
private fun summaryTextOf(summary: HomeSummary): AnnotatedString {
    val parts = summaryPartsOf(summary).map { (part, count) ->
        count to pluralStringResource(
            when (part) {
                SummaryPart.MENTIONS -> R.plurals.home_summary_mentions
                SummaryPart.UNREAD -> R.plurals.home_summary_unread
                SummaryPart.THREAD_REPLIES -> R.plurals.home_summary_thread_replies
                SummaryPart.ACTIVE_CALLS -> R.plurals.home_summary_active_calls
                SummaryPart.AROUND -> R.plurals.home_summary_around
            },
            count,
        )
    }
    val colors = Waddle.colors
    return buildAnnotatedString {
        parts.forEachIndexed { index, (count, label) ->
            // No-break spaces keep "2 mentions" whole and the dot on the
            // line it follows, so no line starts with a separator.
            if (index > 0) withStyle(SpanStyle(color = colors.fgSoft)) { append(NBSP + "· ") }
            withStyle(SpanStyle(color = colors.fg, fontWeight = FontWeight.SemiBold)) { append(count.toString()) }
            append(NBSP)
            append(label.replace(' ', NBSP))
        }
    }
}

/**
 * A live huddle (web call card): ember border and glow, "Live now" or
 * "You're here", the room, who is in it, and Join. Tapping the card
 * opens the room.
 */
@Composable
fun LiveHuddleCard(
    room: RoomActivity,
    onOpen: () -> Unit,
    onJoin: () -> Unit,
    modifier: Modifier = Modifier,
    speaking: Set<String> = emptySet(),
) {
    val colors = Waddle.colors
    val names = room.huddle.map { it.nick }
    val preview = participantPreviewOf(names)
    HuddleCard(
        modifier = modifier.fillMaxWidth(),
        live = true,
        onClick = onOpen,
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
        ) {
            Icon(
                if (room.video) Icons.Outlined.Videocam else Icons.Outlined.Call,
                contentDescription = null,
                tint = colors.liveText,
                modifier = Modifier.size(KICKER_GLYPH),
            )
            Kicker(
                text = stringResource(if (room.youAreInHuddle) R.string.home_call_here else R.string.home_call_live),
                color = colors.liveText,
            )
        }
        Spacer(Modifier.size(Waddle.space.xs))
        RoomTitle(name = room.name, forum = room.forum)
        Spacer(Modifier.size(Waddle.space.sm))
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
        ) {
            HuddleStack(participants = room.huddle, speaking = speaking, max = CARD_STACK)
            Text(
                text = preview,
                style = MaterialTheme.typography.bodySmall,
                color = colors.fgMuted,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        Spacer(Modifier.size(Waddle.space.md))
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        ) {
            if (room.canJoin) {
                val join = stringResource(R.string.room_join_description, room.name)
                PillButton(
                    text = stringResource(R.string.room_join),
                    onClick = onJoin,
                    tone = PillTone.LIVE,
                    modifier = Modifier.semantics { contentDescription = join },
                )
            } else {
                PillButton(text = stringResource(R.string.home_call_open), onClick = onOpen, tone = PillTone.QUIET)
            }
            val inCall = stringResource(R.string.home_call_in_this_call, names.size)
            CountBadge(
                count = names.size,
                tone = CountTone.LIVE,
                modifier = Modifier.clearAndSetSemantics { contentDescription = inCall },
            )
            Text(
                text = stringResource(if (room.video) R.string.home_call_video else R.string.home_call_voice),
                style = MaterialTheme.typography.labelMedium,
                color = colors.fgMuted,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

/** A busy room under "Happening now": what is unread, the newest line, a quiet way in. */
@Composable
fun BusyRoomCard(room: RoomActivity, now: ZonedDateTime, onOpen: () -> Unit, modifier: Modifier = Modifier) {
    val colors = Waddle.colors
    HuddleCard(modifier = modifier.fillMaxWidth(), onClick = onOpen) {
        Kicker(
            text = when {
                room.mentions > 0 -> pluralStringResource(R.plurals.room_kicker_mentions, room.mentions, room.mentions)
                else -> stringResource(R.string.room_kicker_unread, room.loudUnread)
            },
            color = if (room.mentions > 0) colors.liveText else colors.fgMuted,
        )
        Spacer(Modifier.size(Waddle.space.xs))
        RoomTitle(name = room.name, forum = room.forum)
        roomBodyOf(room)?.let { body ->
            Spacer(Modifier.size(Waddle.space.xxs))
            Text(
                text = body,
                style = MaterialTheme.typography.bodyMedium,
                color = colors.fgMuted,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
        Spacer(Modifier.size(Waddle.space.sm))
        Row(verticalAlignment = Alignment.CenterVertically) {
            PillButton(text = stringResource(R.string.room_enter), onClick = onOpen, tone = PillTone.QUIET)
            Spacer(Modifier.weight(1f))
            room.lastUpdated?.let { millis ->
                Text(
                    text = listTimeOf(Instant.ofEpochMilli(millis), now.zone, now.toLocalDate()),
                    style = Waddle.type.time,
                    color = colors.fgMuted,
                )
            }
        }
    }
}

/**
 * A room where someone expects you (web "Needs someone like you" row):
 * the room's initial on teal, the name with an ember @count and a
 * replies chip, the newest line, and in ember what is waiting.
 */
@Composable
fun NeedsYouRow(room: RoomActivity, onOpen: () -> Unit, modifier: Modifier = Modifier) {
    val colors = Waddle.colors
    val shape = RoundedCornerShape(Waddle.radius.panel)
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = modifier
            .fillMaxWidth()
            .clip(shape)
            .background(colors.surface)
            .border(1.dp, colors.border, shape)
            .clickable(role = Role.Button, onClick = onOpen)
            .heightIn(min = Waddle.control.touch)
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.sm),
    ) {
        Box(
            contentAlignment = Alignment.Center,
            modifier = Modifier
                .size(Waddle.control.md)
                .background(colors.actionSubtle, CircleShape),
        ) {
            Text(
                text = room.name.trim().take(1).uppercase().ifEmpty { "#" },
                style = MaterialTheme.typography.titleSmall,
                fontWeight = FontWeight.Bold,
                color = colors.actionText,
            )
        }
        Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs)) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
            ) {
                Text(
                    text = room.name,
                    style = Waddle.type.control,
                    fontWeight = FontWeight.SemiBold,
                    color = colors.fg,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f, fill = false),
                )
                if (room.mentions > 0) {
                    CountBadge(
                        count = room.mentions,
                        tone = CountTone.LIVE,
                        prefix = "@",
                        modifier = Modifier.clearAndSetSemantics { },
                    )
                }
                if (room.threadUnread > 0) RepliesChip(room.threadUnread)
            }
            roomBodyOf(room)?.let {
                Text(
                    text = it,
                    style = MaterialTheme.typography.bodySmall,
                    color = colors.fgMuted,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Text(text = waitingLineOf(room), style = MaterialTheme.typography.bodySmall, color = colors.liveText)
        }
        Box(
            contentAlignment = Alignment.Center,
            modifier = Modifier
                .clearAndSetSemantics { }
                .heightIn(min = Waddle.control.sm)
                .border(1.dp, colors.border, RoundedCornerShape(Waddle.radius.pill))
                .padding(horizontal = Waddle.space.sm),
        ) {
            Text(text = stringResource(R.string.home_needs_open), style = Waddle.type.control, color = colors.fg)
        }
    }
}

@Composable
private fun RepliesChip(count: Int) {
    Box(
        contentAlignment = Alignment.Center,
        modifier = Modifier
            .clearAndSetSemantics { }
            .border(1.dp, Waddle.colors.border, RoundedCornerShape(Waddle.radius.pill))
            .padding(horizontal = Waddle.space.xs),
    ) {
        Text(
            text = pluralStringResource(R.plurals.home_needs_replies, count, count),
            style = Waddle.type.time,
            color = Waddle.colors.fgMuted,
            maxLines = 1,
        )
    }
}

/** "2 mentions and 1 thread reply waiting". */
@Composable
private fun waitingLineOf(room: RoomActivity): String {
    val mentions = room.mentions.takeIf { it > 0 }
        ?.let { pluralStringResource(R.plurals.home_needs_mentions, it, it) }
    val replies = room.threadUnread.takeIf { it > 0 }
        ?.let { pluralStringResource(R.plurals.home_needs_thread_replies, it, it) }
    val what = if (mentions != null && replies != null) {
        stringResource(R.string.home_needs_both, mentions, replies)
    } else {
        mentions ?: replies.orEmpty()
    }
    return stringResource(R.string.home_needs_waiting, what)
}

/** A card's worth of placeholder for "Happening now" while the topology loads. */
@Composable
fun SkeletonCard(modifier: Modifier = Modifier) {
    HuddleCard(modifier = modifier.fillMaxWidth()) {
        SkeletonBlock(Modifier.fillMaxWidth(SKELETON_KICKER))
        Spacer(Modifier.size(Waddle.space.xs))
        SkeletonBlock(Modifier.fillMaxWidth(SKELETON_TITLE), height = SKELETON_TITLE_HEIGHT)
        Spacer(Modifier.size(Waddle.space.xs))
        SkeletonBlock(Modifier.fillMaxWidth(SKELETON_LINE_LONG))
    }
}

private const val NBSP = '\u00A0'
private const val DATE_SKELETON = "EEEdMMM"
private const val CARD_STACK = 3
private const val SKELETON_LINE_LONG = 0.85f
private const val SKELETON_LINE_SHORT = 0.5f
private const val SKELETON_KICKER = 0.35f
private const val SKELETON_TITLE = 0.55f
private val SKELETON_TITLE_HEIGHT = 18.dp
private val SLOT_WIDTH = 52.dp
private val MASCOT = 80.dp
private val KICKER_GLYPH = 14.dp
