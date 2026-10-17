package social.waddle.android.feature.conversation

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.waitForUpOrCancellation
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowRight
import androidx.compose.material.icons.outlined.DeleteOutline
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.PushPin
import androidx.compose.material.icons.outlined.Schedule
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.minimumInteractiveComponentSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.dp
import social.waddle.android.R
import social.waddle.android.avatar.PeerAvatar
import social.waddle.android.client.store.ReactionGroup
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.jid.localpartOf
import social.waddle.android.jid.resourcepartOf
import social.waddle.android.theme.Waddle
import social.waddle.android.theme.WaddleTokens
import social.waddle.android.ui.huddle.AvatarSize

/** Where a message is on its way to the server, when that is worth saying. */
@Immutable
sealed interface Delivery {
    data object Sending : Delivery

    data object Queued : Delivery

    data class Failed(val localId: Long) : Delivery

    /** The server bounced a stored row; nothing to retry from here. */
    data object Rejected : Delivery
}

/** The quiet marks after the time on an author line (web meta line). */
@Immutable
data class MetaMarks(
    val edited: Boolean = false,
    val pinned: Boolean = false,
    val delivery: Delivery? = null,
) {
    val isEmpty: Boolean get() = !edited && !pinned && delivery == null
}

/** Pin · (edited) · delivery, as items of the author line's flow. */
@Composable
internal fun MetaMarkItems(marks: MetaMarks, onRetry: (Long) -> Unit) {
    val colors = Waddle.colors
    if (marks.pinned) {
        Icon(
            Icons.Outlined.PushPin,
            contentDescription = stringResource(R.string.message_pinned),
            tint = colors.fgMuted,
            modifier = Modifier.size(META_ICON),
        )
    }
    if (marks.edited) MetaText(stringResource(R.string.message_edited), colors.fgMuted)
    when (val delivery = marks.delivery) {
        null -> Unit
        Delivery.Sending -> MetaText(stringResource(R.string.message_sending), colors.fgMuted)
        Delivery.Queued ->
            MetaText(stringResource(R.string.message_queued), colors.warningText, Icons.Outlined.Schedule)
        Delivery.Rejected ->
            MetaText(stringResource(R.string.message_delivery_failed), colors.dangerText, Icons.Outlined.ErrorOutline)
        is Delivery.Failed -> {
            MetaText(stringResource(R.string.message_delivery_failed), colors.dangerText, Icons.Outlined.ErrorOutline)
            Text(
                text = stringResource(R.string.message_retry),
                style = MetaStyle.copy(fontWeight = FontWeight.SemiBold),
                color = colors.actionText,
                modifier = Modifier
                    .clip(RoundedCornerShape(Waddle.radius.tag))
                    .clickable(role = Role.Button) { onRetry(delivery.localId) }
                    .padding(horizontal = Waddle.space.xxs),
            )
        }
    }
}

/** Marks on their own line, for rows without an author line. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun MetaLine(marks: MetaMarks, onRetry: (Long) -> Unit = {}) {
    FlowRow(
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
        itemVerticalAlignment = Alignment.CenterVertically,
    ) { MetaMarkItems(marks, onRetry) }
}

@Composable
private fun MetaText(text: String, color: Color, icon: ImageVector? = null) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
    ) {
        icon?.let { Icon(it, contentDescription = null, tint = color, modifier = Modifier.size(META_ICON)) }
        Text(text = text, style = MetaStyle, color = color, maxLines = 1)
    }
}

/** "This message was deleted.", dimmed (web: .55, trash icon, italic). */
@Composable
internal fun TombstoneLine() {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
        modifier = Modifier.alpha(TOMBSTONE_ALPHA),
    ) {
        Icon(
            Icons.Outlined.DeleteOutline,
            contentDescription = null,
            tint = Waddle.colors.fgMuted,
            modifier = Modifier.size(Waddle.space.md),
        )
        Text(
            text = stringResource(R.string.message_deleted),
            style = Waddle.type.body,
            fontStyle = FontStyle.Italic,
            color = Waddle.colors.fgMuted,
        )
    }
}

/**
 * XEP-0461 reply chip: a 3dp teal bar, "↳ @name" and one italic line of
 * the parent. Tap jumps to it when it is loaded.
 */
@Composable
internal fun ReplyChip(parent: TimelineItem?, fallbackSender: String?, onClick: () -> Unit) {
    val colors = Waddle.colors
    // A MUC occupant JID's display name is its resource (nick).
    val author = parent?.let { authorNameOf(it) }
        ?: fallbackSender?.let { resourcepartOf(it) ?: localpartOf(it) }
    val quote = when {
        parent == null -> stringResource(R.string.reply_original_unloaded)
        parent.tombstone != null -> stringResource(R.string.message_deleted)
        else -> parent.body.lineSequence().firstOrNull { it.isNotBlank() }.orEmpty()
    }
    val line = buildAnnotatedString {
        if (author != null) {
            withStyle(SpanStyle(fontWeight = FontWeight.SemiBold, color = colors.fg)) {
                append(stringResource(R.string.reply_chip_author, author))
            }
            append("  ")
        }
        withStyle(SpanStyle(fontStyle = FontStyle.Italic)) { append(quote) }
    }
    Row(
        modifier = Modifier
            .height(IntrinsicSize.Min)
            .clip(RoundedCornerShape(Waddle.radius.tag))
            // Unloaded, the quote is inert but still keeps the tap from
            // reaching the post (which would open its thread).
            .then(if (parent != null) Modifier.clickable(onClick = onClick) else Modifier.swallowTaps())
            .padding(vertical = Waddle.space.xxs),
    ) {
        Box(
            Modifier
                .width(QUOTE_BAR)
                .fillMaxHeight()
                .background(colors.action.copy(alpha = QUOTE_BAR_ALPHA), RoundedCornerShape(QUOTE_BAR)),
        )
        Text(
            text = line,
            style = MetaStyle,
            color = colors.fgMuted,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.padding(start = Waddle.space.xs),
        )
    }
}

/**
 * XEP-0444 reactions (web `.chat-reaction`): 28dp pills, radius 8, a mono
 * count. Your own reaction is teal; others warm towards teal as more
 * people join in. Tap toggles yours. The pill stays 28dp to keep the
 * row's rhythm; Compose extends its touch area to 48dp.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun ReactionPills(reactions: List<ReactionGroup>, onToggle: (String) -> Unit) {
    val haptics = LocalHapticFeedback.current
    FlowRow(
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
        verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
        modifier = Modifier.padding(top = Waddle.space.xxs),
    ) {
        reactions.forEach { group ->
            ReactionPill(group) {
                haptics.performHapticFeedback(HapticFeedbackType.TextHandleMove)
                onToggle(group.emoji)
            }
        }
    }
}

@Composable
private fun ReactionPill(group: ReactionGroup, onClick: () -> Unit) {
    val colors = Waddle.colors
    val shape = RoundedCornerShape(WaddleTokens.Radius.sm)
    val fill = when {
        group.mine -> colors.action.copy(alpha = OWN_FILL_ALPHA)
        else -> colors.action.copy(alpha = warmthOf(group.count))
    }
    val border = if (group.mine) colors.action.copy(alpha = OWN_BORDER_ALPHA) else colors.border
    val description = pluralStringResource(
        if (group.mine) R.plurals.reaction_description_mine else R.plurals.reaction_description,
        group.count,
        group.emoji,
        group.count,
    )
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
        // 48dp touch target around the 28dp pill: the message row's
        // long-press would otherwise take every near miss.
        modifier = Modifier
            .minimumInteractiveComponentSize()
            .height(REACTION_HEIGHT)
            .clip(shape)
            .background(colors.surface2, shape)
            .background(fill, shape)
            .border(1.dp, border, shape)
            .clickable(role = Role.Button, onClick = onClick)
            .semantics(mergeDescendants = true) { contentDescription = description }
            .padding(horizontal = Waddle.space.xs),
    ) {
        Text(text = group.emoji, style = Waddle.type.control)
        Text(
            text = group.count.toString(),
            style = Waddle.type.time,
            color = if (group.mine) colors.actionText else colors.fgMuted,
        )
    }
}

/** Web reaction warmth: more people, a little more teal (2, 3, 5+). */
private fun warmthOf(count: Int): Float = when {
    count >= WARM_HIGH -> WARMTH_HIGH
    count >= WARM_MID -> WARMTH_MID
    count >= WARM_LOW -> WARMTH_LOW
    else -> 0f
}

/**
 * The attached reply card under a thread root: the repliers' faces, the
 * count in teal and how long ago the last reply came, then the newest
 * reply as one muted line. One button, at least 48dp tall; a teal rail
 * from the avatar gutter ties it to the post.
 */
@Composable
internal fun ReplyCard(count: Int, activity: ThreadActivity?, onClick: () -> Unit) {
    val colors = Waddle.colors
    val clock = rememberConversationClock()
    val shape = RoundedCornerShape(Waddle.radius.card)
    val countText = pluralStringResource(R.plurals.thread_card_replies, count, count)
    // Recomputed each minute, so "2m" does not sit still while on screen.
    val minute = rememberMinuteTick()
    val lastReplyAt = activity?.lastReplyAt
    val age = remember(clock, lastReplyAt, minute) { lastReplyAt?.let(clock::ageOf) }
    val latest = activity?.latestReply
    val snippet = latest?.let { snippetText(it.snippet) }
    val spokenAge = age?.let { clock.spokenAge(it) }.orEmpty()
    val description = if (latest != null && snippet != null) {
        val plural = R.plurals.thread_card_description_latest
        pluralStringResource(plural, count, count, latest.author, snippet, spokenAge)
    } else {
        pluralStringResource(R.plurals.thread_card_description, count, count, spokenAge)
    }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
        modifier = Modifier
            .padding(top = Waddle.space.xxs)
            .threadRail(colors.action.copy(alpha = RAIL_ALPHA))
            .fillMaxWidth()
            .heightIn(min = REPLY_CARD_MIN_HEIGHT)
            .clip(shape)
            .background(colors.surface, shape)
            .border(1.dp, colors.border, shape)
            .clickable(
                onClickLabel = stringResource(R.string.action_open_thread),
                role = Role.Button,
                onClick = onClick,
            )
            .clearAndSetSemantics { contentDescription = description }
            .padding(horizontal = Waddle.space.sm, vertical = Waddle.space.xs),
    ) {
        Column(
            verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
            modifier = Modifier.weight(1f),
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
            ) {
                activity?.let { ThreadFaces(it) }
                Text(
                    text = countText,
                    style = MetaStyle.copy(fontWeight = FontWeight.SemiBold),
                    color = colors.actionText,
                    maxLines = 1,
                )
                age?.let {
                    val ageWords = clock.ageText(it)
                    Text(text = "· $ageWords", style = Waddle.type.time, color = colors.fgMuted, maxLines = 1)
                }
            }
            if (latest != null && snippet != null) {
                val line = stringResource(R.string.thread_card_latest, latest.author, snippet)
                val authorStart = line.indexOf(latest.author)
                val authorEnd = authorStart + latest.author.length
                Text(
                    text = buildAnnotatedString {
                        append(line)
                        val bold = SpanStyle(fontWeight = FontWeight.SemiBold)
                        if (authorStart >= 0) addStyle(bold, authorStart, authorEnd)
                    },
                    style = MetaStyle,
                    color = colors.fgMuted,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        Icon(
            Icons.AutoMirrored.Outlined.KeyboardArrowRight,
            contentDescription = null,
            tint = colors.fgMuted,
            modifier = Modifier.size(CHEVRON),
        )
    }
}

/** The words a reply snippet stands for. */
@Composable
private fun snippetText(snippet: ReplySnippet): String = when (snippet) {
    is ReplySnippet.Text -> snippet.text
    ReplySnippet.Photo -> stringResource(R.string.thread_reply_photo)
    ReplySnippet.Attachment -> stringResource(R.string.message_attachment)
    ReplySnippet.Deleted -> stringResource(R.string.thread_reply_deleted)
}

@Composable
private fun ThreadFaces(activity: ThreadActivity) {
    val colors = Waddle.colors
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(-THREAD_FACE_OVERLAP),
    ) {
        activity.participants.forEach { person ->
            PeerAvatar(
                jid = person.jid,
                displayName = person.name,
                size = THREAD_FACE,
                modifier = Modifier.border(FACE_RING, colors.surface, CircleShape),
            )
        }
    }
    if (activity.moreParticipants > 0) {
        Text(
            text = stringResource(R.string.thread_more_faces, activity.moreParticipants),
            style = Waddle.type.time,
            color = colors.fgMuted,
        )
    }
}

/**
 * The teal rail in the avatar gutter: down from the card's top, round
 * the corner and across to the card's edge, drawn outside its bounds.
 */
private fun Modifier.threadRail(color: Color): Modifier = drawBehind {
    val mirrored = layoutDirection == LayoutDirection.Rtl

    // Horizontal positions measured from the card's start edge.
    fun x(fromStart: Float): Float = if (mirrored) size.width - fromStart else fromStart

    val gutter = -(AvatarSize.MESSAGE.dp / 2 + COLUMN_GAP).toPx()
    val y = size.height / 2f
    val reach = ELBOW_REACH.toPx()
    val path = Path().apply {
        moveTo(x(gutter), 0f)
        lineTo(x(gutter), y - reach)
        quadraticTo(x(gutter), y, x(gutter + reach), y)
        lineTo(x(0f), y)
    }
    drawPath(path, color, style = Stroke(width = ELBOW_STROKE.toPx(), cap = StrokeCap.Round))
}

/**
 * Takes taps on an inert child so they do not fall through to the row,
 * where a tap opens the thread. A long press still reaches the row.
 */
internal fun Modifier.swallowTaps(): Modifier = pointerInput(Unit) {
    awaitEachGesture {
        awaitFirstDown(requireUnconsumed = false)
        waitForUpOrCancellation()?.consume()
    }
}

/** Meta and chip text: Outfit 12. */
internal val MetaStyle
    @Composable get() = Waddle.type.control.copy(
        fontSize = WaddleTokens.Huddle.FontSize.caption,
        fontWeight = FontWeight.Normal,
    )

private val META_ICON = 12.dp
private val QUOTE_BAR = 3.dp
private val REACTION_HEIGHT = Waddle.control.xs
private val THREAD_FACE = 18.dp
private val THREAD_FACE_OVERLAP = 4.dp
private val FACE_RING = 1.5.dp
private val COLUMN_GAP = WaddleTokens.Space.sm
private val ELBOW_REACH = 8.dp
private val ELBOW_STROKE = 1.5.dp
private val CHEVRON = 18.dp

/** The card is one button: a full touch target even on one line. */
private val REPLY_CARD_MIN_HEIGHT = 48.dp
private const val TOMBSTONE_ALPHA = 0.55f
private const val QUOTE_BAR_ALPHA = 0.55f
private const val OWN_FILL_ALPHA = 0.14f
private const val OWN_BORDER_ALPHA = 0.4f
private const val RAIL_ALPHA = 0.55f
private const val WARM_LOW = 2
private const val WARM_MID = 3
private const val WARM_HIGH = 5
private const val WARMTH_LOW = 0.04f
private const val WARMTH_MID = 0.07f
private const val WARMTH_HIGH = 0.1f
