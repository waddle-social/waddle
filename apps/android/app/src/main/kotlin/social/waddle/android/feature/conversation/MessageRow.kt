package social.waddle.android.feature.conversation

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.layout.layout
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.text
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import social.waddle.android.R
import social.waddle.android.client.AuthorBadge
import social.waddle.android.client.AuthorBadgeKind
import social.waddle.android.client.messageAuthorBadgeOf
import social.waddle.android.client.richBlocksOf
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.jid.localpartOf
import social.waddle.android.jid.resourcepartOf
import social.waddle.android.theme.Waddle
import social.waddle.android.theme.WaddleTokens
import social.waddle.android.ui.huddle.AvatarSize
import social.waddle.android.ui.huddle.HuddleAvatar
import social.waddle.client.ffi.WaddlePresence
import java.time.Instant

/** Everything a timeline row can do, bundled so rows take one parameter. */
class MessageRowActions(
    val onLongPress: (TimelineItem) -> Unit = {},
    val onToggleReaction: (TimelineItem, String) -> Unit = { _, _ -> },
    val onOpenThread: ((TimelineItem) -> Unit)? = null,
    val onQuoteClick: (TimelineItem) -> Unit = {},
    val onRetry: (Long) -> Unit = {},
    /** Avatar or author name tapped; `null` leaves them inert. */
    val onOpenProfile: ((MessageAuthor) -> Unit)? = null,
)

/** Context shared by every row of one conversation. */
class MessageRowContext(
    val selfBareJid: String? = null,
    /** MUC occupant presence by nick (XEP-0317 hats / XEP-0045 badges). */
    val authorPresence: Map<String, WaddlePresence> = emptyMap(),
    /** Bare JIDs the server declares as bots: their rows carry the BOT badge. */
    val declaredBotJids: Set<String> = emptySet(),
    /** Origin whose cached XEP-0363 preview images may load (web parity). */
    val trustedMediaOrigin: String? = null,
)

/**
 * One feed row (web `messages.css`): a 40dp avatar column, then the
 * author line — name in the foreground colour, a role hat, the time and
 * the meta marks — on the first message of an author burst, then the
 * body. Every author, the reader included, sits on the same left edge.
 * A row that mentions the reader carries a 3dp ember bar over an ember
 * wash. [endsGroup] closes a burst with the full 12dp of air.
 */
@Composable
fun MessageRow(
    entry: FeedEntry,
    actions: MessageRowActions,
    context: MessageRowContext,
    modifier: Modifier = Modifier,
    endsGroup: Boolean = true,
) {
    when (val row = entry.row) {
        is ConversationRow.Stored -> StoredMessageRow(entry, row.item, actions, context, endsGroup, modifier)
        is ConversationRow.Unconfirmed -> PendingMessageRow(entry, row.message, actions, context, endsGroup, modifier)
    }
}

/** What tapping a row and its author does; `null` leaves that part inert. */
private class RowTaps(
    val onLongPress: (() -> Unit)?,
    /** The post itself: opens its thread when it has replies. */
    val onOpenThread: (() -> Unit)?,
    val onOpenProfile: (() -> Unit)?,
)

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun RowFrame(
    entry: FeedEntry,
    author: String,
    endsGroup: Boolean,
    taps: RowTaps,
    modifier: Modifier = Modifier,
    header: @Composable () -> Unit,
    content: @Composable () -> Unit,
) {
    val haptics = LocalHapticFeedback.current
    val colors = Waddle.colors
    val onLongPress = taps.onLongPress
    // The avatar and name are hidden from screen readers (the name stays
    // in the row's text); the row offers their "open profile" instead.
    val profileAction = taps.onOpenProfile?.let { open ->
        CustomAccessibilityAction(stringResource(R.string.author_open_profile, author)) {
            open()
            true
        }
    }
    Row(
        modifier = modifier
            .fillMaxWidth()
            .then(if (entry.mentionsSelf) Modifier.mentionWash(colors.live) else Modifier)
            .then(
                if (onLongPress != null) {
                    // Touch: a tap on a post with replies opens its
                    // thread; children with their own clicks (links,
                    // chips, media, reactions, the quote, the author)
                    // take theirs first.
                    Modifier.combinedClickable(
                        onClick = taps.onOpenThread ?: {},
                        onClickLabel = taps.onOpenThread?.let { stringResource(R.string.action_open_thread) },
                        onLongClickLabel = stringResource(R.string.message_actions_label),
                        onLongClick = {
                            haptics.performHapticFeedback(HapticFeedbackType.LongPress)
                            onLongPress()
                        },
                    )
                } else {
                    Modifier
                },
            )
            .semantics(mergeDescendants = true) {
                if (profileAction != null) customActions = listOf(profileAction)
            }
            .padding(
                start = Waddle.space.md,
                end = Waddle.space.md,
                top = if (entry.startsGroup) Waddle.space.sm else ROW_GAP,
                bottom = if (endsGroup) Waddle.space.sm else ROW_GAP,
            ),
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
    ) {
        Box(modifier = Modifier.width(AvatarSize.MESSAGE.dp)) {
            if (entry.startsGroup) AuthorAvatar(jid = entry.authorJid, author = author, onClick = taps.onOpenProfile)
        }
        Column(
            modifier = Modifier.weight(1f),
            verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
        ) {
            if (entry.startsGroup) header()
            content()
        }
    }
}

/**
 * The 40dp author face. Tappable, it opens their profile through a 48dp
 * target that overhangs the gutter without widening it. Screen readers
 * skip it: the row carries the profile action.
 */
@Composable
private fun AuthorAvatar(jid: String?, author: String, onClick: (() -> Unit)?) {
    if (onClick == null) {
        HuddleAvatar(jid = jid, name = author, size = AvatarSize.MESSAGE)
        return
    }
    Box(modifier = Modifier.clearAndSetSemantics {}) {
        Box(
            contentAlignment = Alignment.Center,
            modifier = Modifier
                .overhang(visible = AvatarSize.MESSAGE.dp, target = MIN_TOUCH_TARGET)
                .clip(CircleShape)
                .clickable(onClick = onClick),
        ) {
            HuddleAvatar(jid = jid, name = author, size = AvatarSize.MESSAGE)
        }
    }
}

/**
 * Measures the content at [target] but takes only [visible] of layout,
 * centred: a full touch target around a smaller face, with the layout
 * (and the row rhythm) unchanged.
 */
private fun Modifier.overhang(visible: Dp, target: Dp): Modifier = layout { measurable, _ ->
    val targetPx = target.roundToPx()
    val visiblePx = visible.roundToPx()
    val placeable = measurable.measure(Constraints.fixed(targetPx, targetPx))
    layout(visiblePx, visiblePx) {
        placeable.place((visiblePx - targetPx) / 2, (visiblePx - targetPx) / 2)
    }
}

/**
 * The mention row: a 3dp ember bar at the left edge over an ember wash
 * that is strongest beside the bar. A flat 8% ember over the night ground
 * mixes to grey; letting it fall off from the bar keeps it reading warm.
 */
private fun Modifier.mentionWash(live: Color): Modifier = drawBehind {
    drawRect(live.copy(alpha = MENTION_BASE_ALPHA))
    drawRect(
        Brush.horizontalGradient(
            0f to live.copy(alpha = MENTION_EDGE_ALPHA),
            MENTION_FALLOFF to Color.Transparent,
        ),
    )
    drawRect(live, size = size.copy(width = MENTION_BAR.toPx()))
}

@Composable
private fun StoredMessageRow(
    entry: FeedEntry,
    item: TimelineItem,
    actions: MessageRowActions,
    context: MessageRowContext,
    endsGroup: Boolean,
    modifier: Modifier,
) {
    val author = authorNameOf(item)
    val groupchat = isGroupchatRow(item)
    val profileAuthor = MessageAuthor(name = author, jid = entry.authorJid, nick = author.takeIf { groupchat })
    val openProfile = actions.onOpenProfile?.let { open -> { open(profileAuthor) } }
    val openThread = actions.onOpenThread?.takeIf { entry.replyCount > 0 }?.let { open -> { open(item) } }
    val marks = MetaMarks(
        edited = item.edited && item.tombstone == null,
        pinned = entry.isPinned,
        delivery = if (item.rejected) Delivery.Rejected else null,
    )
    RowFrame(
        entry = entry,
        author = author,
        endsGroup = endsGroup,
        taps = RowTaps(
            onLongPress = if (item.rejected) null else ({ actions.onLongPress(item) }),
            onOpenThread = openThread,
            onOpenProfile = openProfile,
        ),
        modifier = modifier,
        header = {
            MessageHeader(
                author = author,
                onAuthorClick = openProfile,
                badge = if (groupchat) {
                    messageAuthorBadgeOf(item.authorJid, context.declaredBotJids) { context.authorPresence[author] }
                } else {
                    null
                },
                sentAt = entry.sentAt,
                marks = marks,
            )
        },
    ) {
        item.replyToId?.let {
            ReplyChip(
                parent = entry.replyParent,
                fallbackSender = item.replyToSender,
                fallbackQuote = entry.replyFallbackQuote,
                onClick = { entry.replyParent?.let(actions.onQuoteClick) },
            )
        }
        if (item.tombstone != null) {
            TombstoneLine()
        } else {
            StoredMessageBody(entry, item, marks, actions, context)
        }
        // A deleted root keeps its card: the replies are still there.
        if (openThread != null) {
            ReplyCard(count = entry.replyCount, activity = entry.threadActivity, onClick = openThread)
        }
    }
}

/** A live (not deleted) stored message: content, marks, reactions and scores. */
@Composable
private fun StoredMessageBody(
    entry: FeedEntry,
    item: TimelineItem,
    marks: MetaMarks,
    actions: MessageRowActions,
    context: MessageRowContext,
) {
    MessageContent(item, context)
    // A continuation row has no author line to carry its marks.
    if (!entry.startsGroup && !marks.isEmpty) MetaLine(marks)
    if (item.reactions.isNotEmpty()) {
        ReactionPills(item.reactions) { emoji -> actions.onToggleReaction(item, emoji) }
    }
    item.safetyScores?.let { scores ->
        val category = safetyScoreChipCategory(scores) ?: return@let
        var scoresOpen by remember { mutableStateOf(false) }
        SafetyScoresChip(category = category, onClick = { scoresOpen = true })
        if (scoresOpen) SafetyScoresSheet(scores = scores, onDismiss = { scoresOpen = false })
    }
}

@Composable
private fun PendingMessageRow(
    entry: FeedEntry,
    message: PendingMessage,
    actions: MessageRowActions,
    context: MessageRowContext,
    endsGroup: Boolean,
    modifier: Modifier,
) {
    // The name the server echo will carry, so the row does not change
    // name when it is confirmed.
    val author = context.selfBareJid?.let(::localpartOf) ?: stringResource(R.string.message_author_me)
    val profileAuthor = MessageAuthor(name = author, jid = entry.authorJid, nick = null)
    val openProfile = actions.onOpenProfile?.let { open -> { open(profileAuthor) } }
    val marks = MetaMarks(
        delivery = when {
            message.failed -> Delivery.Failed(message.localId)
            message.queued -> Delivery.Queued
            !message.acked -> Delivery.Sending
            else -> null
        },
    )
    RowFrame(
        entry = entry,
        author = author,
        endsGroup = endsGroup,
        taps = RowTaps(onLongPress = null, onOpenThread = null, onOpenProfile = openProfile),
        modifier = modifier,
        header = {
            MessageHeader(
                author = author,
                onAuthorClick = openProfile,
                badge = null,
                sentAt = entry.sentAt,
                marks = marks,
                onRetry = actions.onRetry,
            )
        },
    ) {
        // Dimmed until the server has it: the row is real but not yet shared.
        Text(
            text = message.body,
            style = Waddle.type.body,
            color = Waddle.colors.fg,
            modifier = Modifier.alpha(if (message.acked) 1f else PENDING_ALPHA),
        )
        if (!entry.startsGroup && !marks.isEmpty) MetaLine(marks, onRetry = actions.onRetry)
    }
}

/** Name · hat · time · marks; wraps under a long name instead of squeezing it. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun MessageHeader(
    author: String,
    onAuthorClick: (() -> Unit)?,
    badge: AuthorBadge?,
    sentAt: Instant,
    marks: MetaMarks,
    onRetry: (Long) -> Unit = {},
) {
    val clock = rememberConversationClock()
    FlowRow(
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
        itemVerticalAlignment = Alignment.CenterVertically,
    ) {
        AuthorName(author = author, onClick = onAuthorClick)
        badge?.let { HatTag(it) }
        Text(
            text = clock.time(sentAt),
            style = Waddle.type.time,
            color = Waddle.colors.fgMuted,
            maxLines = 1,
        )
        MetaMarkItems(marks, onRetry)
    }
}

/**
 * The author's name. Tappable, it opens their profile; to a screen
 * reader it stays plain text in the row, which carries that action.
 */
@Composable
private fun AuthorName(author: String, onClick: (() -> Unit)?) {
    Box(modifier = Modifier.clearAndSetSemantics { text = AnnotatedString(author) }) {
        Text(
            text = author,
            style = AuthorStyle,
            color = Waddle.colors.fg,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            // Compose widens the touch area of this short line to 48dp.
            modifier = if (onClick != null) {
                Modifier
                    .clip(RoundedCornerShape(Waddle.radius.tag))
                    .clickable(onClick = onClick)
            } else {
                Modifier
            },
        )
    }
}

/** A role hat after the name: mono, uppercase, text only (web `.chat-hat-tag`). */
@Composable
private fun HatTag(badge: AuthorBadge) {
    val colors = Waddle.colors
    val color = when (badge.kind) {
        AuthorBadgeKind.OWNER -> colors.goldText
        AuthorBadgeKind.ADMIN, AuthorBadgeKind.MODERATOR, AuthorBadgeKind.VERIFIED -> colors.actionText
        AuthorBadgeKind.BOT -> colors.mossText
        AuthorBadgeKind.HAT -> colors.fgMuted
    }
    Text(
        text = badge.label.uppercase(),
        style = Waddle.type.kicker,
        color = color,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
        modifier = Modifier.widthIn(max = HAT_MAX_WIDTH),
    )
}

/** Body, media, files and link cards, in web `displayBody` precedence. */
@Composable
private fun MessageContent(item: TimelineItem, context: MessageRowContext) {
    val stickerFile = stickerFileOf(item)
    val inlineImageUrl = inlineImageUrlOf(item, stickerFile)
    // Sticker alt-text, lone image-URL bodies and bodies echoing an
    // attachment URL never render as text.
    val bodyHidden = item.body.isBlank() ||
        stickerFile != null ||
        inlineImageUrl != null ||
        item.sharedFiles.any { it.url == item.body.trim() }
    val blocks = remember(item) {
        if (bodyHidden) {
            emptyList()
        } else {
            richBlocksOf(
                body = item.body,
                markupSpans = item.markupSpans,
                references = item.source.references,
                fallbackStart = item.source.replyFallbackStart,
                fallbackEnd = item.source.replyFallbackEnd,
            )
        }
    }
    if (blocks.isNotEmpty()) RichMessageBody(blocks)
    MessageAttachments(item, stickerFile, inlineImageUrl)
    item.linkPreviews.forEach { preview -> LinkPreviewCard(preview, context.trustedMediaOrigin) }
}

/** Display name: the occupant nick in a MUC, the localpart in 1:1. */
@Composable
internal fun authorNameOf(item: TimelineItem): String {
    val from = item.from ?: return stringResource(R.string.message_unknown_sender)
    return if (isGroupchatRow(item)) resourcepartOf(from) ?: localpartOf(from) else localpartOf(from)
}

/** Author line name: body 13/600 (web `.chat-author`). */
internal val AuthorStyle: TextStyle
    @Composable get() = Waddle.type.control.copy(
        fontSize = WaddleTokens.Huddle.FontSize.control,
        fontWeight = FontWeight.SemiBold,
    )

private val ROW_GAP: Dp = 2.dp
private val MIN_TOUCH_TARGET: Dp = 48.dp
private val MENTION_BAR: Dp = 3.dp
private val HAT_MAX_WIDTH: Dp = 120.dp
private const val MENTION_BASE_ALPHA = 0.06f
private const val MENTION_EDGE_ALPHA = 0.12f
private const val MENTION_FALLOFF = 0.7f
private const val PENDING_ALPHA = 0.72f
