package social.waddle.android.ui

import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.NotificationsOff
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.material3.TopAppBarScrollBehavior
import androidx.compose.runtime.Composable
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.text
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import social.waddle.android.R
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.huddle.CountBadge
import social.waddle.android.ui.huddle.CountTone
import social.waddle.android.ui.huddle.LiveDot
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle

/**
 * The app's top bar: the screen ground (no tonal tint), the Huddle
 * title style (Space Grotesk 18/600) and a heading for screen readers,
 * so every screen's title lines up and is reachable by heading.
 * Scrolled content lifts it onto the rail colour.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun WaddleTopBar(
    title: @Composable () -> Unit,
    modifier: Modifier = Modifier,
    navigationIcon: @Composable () -> Unit = {},
    actions: @Composable RowScope.() -> Unit = {},
    scrollBehavior: TopAppBarScrollBehavior? = null,
) {
    val colors = Waddle.colors
    TopAppBar(
        title = { Box(modifier = Modifier.semantics { heading() }) { title() } },
        navigationIcon = navigationIcon,
        actions = actions,
        modifier = modifier,
        scrollBehavior = scrollBehavior,
        colors = TopAppBarDefaults.topAppBarColors(
            containerColor = colors.bg,
            scrolledContainerColor = colors.bgRail,
            titleContentColor = colors.fg,
            navigationIconContentColor = colors.fg,
            actionIconContentColor = colors.fgMuted,
        ),
    )
}

/**
 * Unread count capsule (Huddle `count`): neutral for plain unread, ember
 * when [emphasized] — only for counts that are for you (mentions).
 */
@Composable
fun UnreadCountBadge(count: Int, modifier: Modifier = Modifier, emphasized: Boolean = false) {
    if (count <= 0) return
    val description = if (emphasized) {
        pluralStringResource(R.plurals.huddle_mentions_count, count, count)
    } else {
        pluralStringResource(R.plurals.huddle_unread_count, count, count)
    }
    CountBadge(
        count = count,
        tone = if (emphasized) CountTone.LIVE else CountTone.NEUTRAL,
        prefix = if (emphasized) MENTION_PREFIX else "",
        modifier = modifier.clearAndSetSemantics { contentDescription = description },
    )
}

/**
 * Text set as a Huddle kicker (mono 11, uppercase, tracked) whose
 * uppercase is presentation only: accessibility and tests read [text] as
 * written, so screen readers do not spell short words out letter by
 * letter.
 */
@Composable
fun KickerText(
    text: String,
    modifier: Modifier = Modifier,
    color: Color = Waddle.colors.fgMuted,
    heading: Boolean = false,
) {
    Text(
        text = text.uppercase(LocalConfiguration.current.locales[0]),
        style = Waddle.type.kicker,
        color = color,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
        modifier = modifier.clearAndSetSemantics {
            this.text = AnnotatedString(text)
            if (heading) heading()
        },
    )
}

/**
 * The label above a group of rows or cards (web `.type-kicker`): mono,
 * uppercase, muted, a heading for screen readers. [live] turns it ember
 * with a live dot, for sections about what is happening now.
 */
@Composable
fun SectionKicker(text: String, modifier: Modifier = Modifier, live: Boolean = false) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
        modifier = modifier.padding(
            start = Waddle.space.md,
            end = Waddle.space.md,
            top = Waddle.space.lg,
            bottom = Waddle.space.xs,
        ),
    ) {
        if (live) LiveDot()
        KickerText(
            text = text,
            color = if (live) Waddle.colors.liveText else Waddle.colors.fgMuted,
            heading = true,
        )
    }
}

/**
 * The head of a bottom sheet, one recipe for every sheet: an optional
 * mono [kicker], the title in Space Grotesk 18/600 (a heading), and an
 * optional muted [supporting] line.
 */
@Composable
fun SheetTitle(
    text: String,
    modifier: Modifier = Modifier,
    supporting: String? = null,
    kicker: String? = null,
) {
    val colors = Waddle.colors
    Column(
        verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
        modifier = modifier
            .fillMaxWidth()
            .padding(start = Waddle.space.md, end = Waddle.space.md, bottom = Waddle.space.md),
    ) {
        kicker?.let { KickerText(text = it) }
        Text(
            text = text,
            style = Waddle.type.title,
            color = colors.fg,
            modifier = Modifier.semantics { heading() },
        )
        supporting?.let {
            Text(
                text = it,
                style = Waddle.type.body.copy(fontSize = Waddle.type.control.fontSize),
                color = colors.fgMuted,
            )
        }
    }
}

/**
 * The conversation open beside the lists (bare JID), or null. Only wide
 * layouts show a list and a conversation together; on phones the list is
 * hidden whenever this is set, so the highlight never shows there.
 */
val LocalOpenConversation = compositionLocalOf<String?> { null }

/**
 * One conversation in a list (rooms, DMs, activity): leading avatar or
 * glyph, title, optional one-line preview, then a meta column whose
 * mono time always sits on the trailing edge, read or unread, with the
 * count under it (two-line rows) or before it (one-line rows).
 *
 * Unread rows read heavier and their time turns foreground; read rows
 * recede. [mentionCount] wins over [unreadCount] and is ember ("@2"):
 * only counts that are for you are ember. The row is one tap target
 * (≥ 56dp, 72dp with a preview) and one merged node for screen readers.
 * The row for [conversationJid] is highlighted while that conversation
 * is open beside the list.
 */
@Composable
fun ConversationListRow(
    title: String,
    leading: @Composable () -> Unit,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    preview: String? = null,
    time: String? = null,
    unreadCount: Int = 0,
    isMuted: Boolean = false,
    conversationJid: String? = null,
    trailingExtra: @Composable () -> Unit = {},
    mentionCount: Int = 0,
) {
    val colors = Waddle.colors
    val unread = (unreadCount > 0 || mentionCount > 0) && !isMuted
    val open = conversationJid != null && conversationJid == LocalOpenConversation.current
    val twoLine = preview != null
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = modifier
            .fillMaxWidth()
            .background(if (open) colors.selection else Color.Transparent)
            .clickable(onClickLabel = stringResource(R.string.huddle_open_action), onClick = onClick)
            .heightIn(min = if (twoLine) TWO_LINE_ROW else ONE_LINE_ROW)
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.xs),
    ) {
        leading()
        Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs)) {
            Text(
                text = title,
                style = Waddle.type.body,
                fontWeight = if (unread) FontWeight.SemiBold else FontWeight.Medium,
                color = if (unread || twoLine) colors.fg else colors.fgMuted,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            if (preview != null) {
                Text(
                    text = preview,
                    style = Waddle.type.control.copy(fontWeight = FontWeight.Normal),
                    color = colors.fgMuted,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        val meta: @Composable () -> Unit = {
            trailingExtra()
            if (isMuted) {
                Icon(
                    Icons.Outlined.NotificationsOff,
                    contentDescription = stringResource(R.string.notify_muted_badge),
                    tint = colors.fgSoft,
                    modifier = Modifier.size(MUTED_ICON),
                )
            }
            when {
                mentionCount > 0 && !isMuted -> UnreadCountBadge(count = mentionCount, emphasized = true)
                else -> UnreadCountBadge(count = unreadCount)
            }
        }
        val timeText: @Composable () -> Unit = {
            time?.let {
                Text(
                    text = it,
                    style = Waddle.type.time,
                    color = if (unread) colors.fg else colors.fgMuted,
                    maxLines = 1,
                )
            }
        }
        if (twoLine) {
            Column(
                horizontalAlignment = Alignment.End,
                verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
            ) {
                timeText()
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
                    modifier = Modifier.heightIn(min = META_LINE),
                ) { meta() }
            }
        } else {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
            ) {
                meta()
                timeText()
            }
        }
    }
}

/**
 * Rounded square glyph tile in place of an avatar (rooms, groups,
 * settings rows). Neutral by default; [tint] and [container] colour it
 * for a state (teal for an invitation, danger for a failure).
 */
@Composable
fun GlyphTile(
    icon: ImageVector,
    modifier: Modifier = Modifier,
    size: Dp = Waddle.control.lg,
    tint: Color = Waddle.colors.fgMuted,
    // Neutral fill that reads on the daylight ground too, where surface2
    // equals the background.
    container: Color = Waddle.colors.neutralBadge,
) {
    val radius = if (size >= Waddle.space.xxl) Waddle.radius.card else Waddle.radius.control
    Box(
        contentAlignment = Alignment.Center,
        modifier = modifier
            .size(size)
            .background(container, RoundedCornerShape(radius)),
    ) {
        Icon(
            icon,
            contentDescription = null,
            tint = tint,
            modifier = Modifier.size(size / 2),
        )
    }
}

/** What an [EmptyState] says about the place it fills. */
enum class EmptyStateTone {
    /** Nothing here yet: a teal invitation. */
    INVITE,

    /** Something failed: a danger tile. */
    PROBLEM,
}

/**
 * Centred empty state (web `.chat-empty`): the mascot ([mascot]) or a
 * glyph tile, a Space Grotesk title that is a heading, and one line
 * that invites rather than reports. [action] sits under the copy.
 */
@Composable
fun EmptyState(
    icon: ImageVector,
    title: String,
    body: String,
    modifier: Modifier = Modifier,
    action: @Composable () -> Unit = {},
    mascot: Boolean = false,
    tone: EmptyStateTone = EmptyStateTone.INVITE,
) {
    val colors = Waddle.colors
    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
        modifier = modifier
            .fillMaxSize()
            .padding(horizontal = Waddle.space.xl, vertical = Waddle.space.lg),
    ) {
        if (mascot) {
            Image(
                painter = painterResource(R.drawable.waddle_mark),
                contentDescription = null,
                modifier = Modifier.size(MASCOT_SIZE),
            )
        } else {
            GlyphTile(
                icon = icon,
                size = EMPTY_TILE,
                tint = if (tone == EmptyStateTone.PROBLEM) colors.dangerText else colors.actionText,
                container = if (tone == EmptyStateTone.PROBLEM) colors.dangerSubtle() else colors.actionSubtle,
            )
        }
        Text(
            text = title,
            style = Waddle.type.title,
            color = colors.fg,
            textAlign = TextAlign.Center,
            modifier = Modifier
                .padding(top = Waddle.space.md)
                .semantics { heading() },
        )
        Text(
            text = body,
            style = Waddle.type.body,
            color = colors.fgMuted,
            textAlign = TextAlign.Center,
            modifier = Modifier
                .widthIn(max = EMPTY_COPY_WIDTH)
                .padding(top = Waddle.space.xs),
        )
        Box(modifier = Modifier.padding(top = Waddle.space.md)) { action() }
    }
}

/** Danger at the web's 10% wash, for failure tiles and banners. */
fun social.waddle.android.theme.WaddleColors.dangerSubtle(): Color = danger.copy(alpha = DANGER_WASH_ALPHA)

/**
 * The `waddle.` wordmark: lowercase Space Grotesk with the full stop in
 * ember — the one brand mark that is allowed to be warm when nothing is
 * live. [style] sets the size (headline by default).
 */
@Composable
fun Wordmark(modifier: Modifier = Modifier, style: TextStyle = Waddle.type.display) {
    val colors = Waddle.colors
    val name = stringResource(R.string.huddle_wordmark)
    Text(
        text = buildAnnotatedString {
            withStyle(SpanStyle(color = colors.fg)) { append(name) }
            withStyle(SpanStyle(color = colors.live)) { append(".") }
        },
        style = style,
        modifier = modifier.semantics { contentDescription = name },
    )
}

/**
 * List time: clock time today, weekday this week, else a short date.
 * [use24Hour] follows the device clock setting when given (pass
 * `DateFormat.is24HourFormat(context)`); `null` falls back to the
 * locale's short time.
 */
fun listTimeOf(
    instant: Instant,
    zone: ZoneId = ZoneId.systemDefault(),
    today: LocalDate = LocalDate.now(zone),
    use24Hour: Boolean? = null,
): String {
    val day = instant.atZone(zone).toLocalDate()
    return when {
        day == today -> when (use24Hour) {
            true -> DateTimeFormatter.ofPattern("HH:mm")
            false -> DateTimeFormatter.ofPattern("h:mm a")
            null -> DateTimeFormatter.ofLocalizedTime(FormatStyle.SHORT)
        }.withZone(zone).format(instant)
        day.isAfter(today.minusDays(WEEK_DAYS)) -> DateTimeFormatter.ofPattern("EEE").withZone(zone).format(instant)
        else -> DateTimeFormatter.ofPattern("d MMM").withZone(zone).format(instant)
    }
}

private const val MENTION_PREFIX = "@"
private const val WEEK_DAYS = 6L
private const val DANGER_WASH_ALPHA = 0.1f
private val ONE_LINE_ROW = 56.dp
private val TWO_LINE_ROW = 72.dp
private val META_LINE = 20.dp
private val MUTED_ICON = 16.dp
private val EMPTY_TILE = 56.dp
private val MASCOT_SIZE = 96.dp
private val EMPTY_COPY_WIDTH = 320.dp
