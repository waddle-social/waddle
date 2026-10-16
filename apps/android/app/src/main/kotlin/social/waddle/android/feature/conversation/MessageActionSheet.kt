package social.waddle.android.feature.conversation

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.Reply
import androidx.compose.material.icons.outlined.AddReaction
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material.icons.outlined.DeleteOutline
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.Forum
import androidx.compose.material.icons.outlined.PushPin
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import social.waddle.android.R
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.huddle.AvatarSize
import social.waddle.android.ui.huddle.HuddleAvatar

/** Web QUICK_REACTION_EMOJIS parity (reaction-mode.ts); the sixth cell opens [MORE_REACTIONS]. */
private val QUICK_REACTIONS = listOf("👍", "❤️", "😂", "🎉", "👀")

/** The wider palette behind the "more" cell. */
private val MORE_REACTIONS = listOf(
    "🙏", "🔥", "✅", "💯", "🚀", "👏",
    "😍", "🤔", "😅", "😢", "😮", "🙌",
    "💡", "👋", "🎯", "🐧", "☕", "⭐",
    "😎", "🤝", "💪", "🫡", "❌", "➕",
)

/**
 * Long-press actions for a timeline row (web action sheet): which
 * message this is, six reaction cells, then 48dp action rows with the
 * destructive ones apart at the bottom. Edit/delete for own messages,
 * copy, pin/unpin in rooms, and XEP-0425 moderation of others' messages
 * for room moderators. [actionable] is false when the row has no usable
 * action target id (e.g. a MUC row without a room-assigned stanza id) —
 * reaction/delete/pin hide, copy stays.
 */
@Composable
fun MessageActionSheet(
    item: TimelineItem,
    actionable: Boolean,
    canPin: Boolean,
    isPinned: Boolean,
    canModerate: Boolean = false,
    onDismiss: () -> Unit,
    onReact: (String) -> Unit,
    onReply: () -> Unit,
    onReplyInThread: (() -> Unit)?,
    onEdit: () -> Unit,
    onRetract: () -> Unit,
    onCopy: () -> Unit,
    onSetPinned: (Boolean) -> Unit,
    onModerate: () -> Unit = {},
) {
    ConversationSheet(onDismissRequest = onDismiss, contentPadding = 0.dp) {
        MessageActionList(
            item = item,
            actionable = actionable,
            canPin = canPin,
            isPinned = isPinned,
            canModerate = canModerate,
            actions = MessageSheetActions(
                onDismiss = onDismiss,
                onReact = onReact,
                onReply = onReply,
                onReplyInThread = onReplyInThread,
                onEdit = onEdit,
                onRetract = onRetract,
                onCopy = onCopy,
                onSetPinned = onSetPinned,
                onModerate = onModerate,
            ),
        )
    }
}

/** Every action the sheet can take; each one also dismisses it. */
internal data class MessageSheetActions(
    val onDismiss: () -> Unit,
    val onReact: (String) -> Unit,
    val onReply: () -> Unit,
    val onReplyInThread: (() -> Unit)?,
    val onEdit: () -> Unit,
    val onRetract: () -> Unit,
    val onCopy: () -> Unit,
    val onSetPinned: (Boolean) -> Unit,
    val onModerate: () -> Unit,
)

/**
 * The sheet body: which message this is about, quick reactions, the
 * everyday actions, then destructive ones apart, then Cancel.
 */
@Composable
internal fun MessageActionList(
    item: TimelineItem,
    actionable: Boolean,
    canPin: Boolean,
    isPinned: Boolean,
    canModerate: Boolean,
    actions: MessageSheetActions,
) {
    val live = item.tombstone == null
    val target = actionable && live
    fun run(action: () -> Unit): () -> Unit = {
        action()
        actions.onDismiss()
    }
    Column {
        MessagePreview(item)
        if (target) QuickReactions(onReact = { emoji -> run { actions.onReact(emoji) }() })
        SheetDivider()
        if (target) {
            SheetAction(Icons.AutoMirrored.Outlined.Reply, stringResource(R.string.action_reply), run(actions.onReply))
        }
        val replyInThread = actions.onReplyInThread
        if (replyInThread != null && live) {
            SheetAction(Icons.Outlined.Forum, stringResource(R.string.action_reply_in_thread), run(replyInThread))
        }
        if (live) {
            SheetAction(Icons.Outlined.ContentCopy, stringResource(R.string.action_copy_message), run(actions.onCopy))
        }
        if (canPin && target) {
            SheetAction(
                Icons.Outlined.PushPin,
                stringResource(if (isPinned) R.string.action_unpin_message else R.string.action_pin_message),
                run { actions.onSetPinned(!isPinned) },
            )
        }
        if (item.isMine && target) {
            SheetAction(Icons.Outlined.Edit, stringResource(R.string.action_edit_message), run(actions.onEdit))
        }
        // XEP-0425: moderators remove OTHERS' messages; own rows retract.
        val retract = item.isMine && target
        val moderate = canModerate && !item.isMine && target
        if (retract || moderate) SheetDivider()
        if (retract) {
            SheetAction(
                Icons.Outlined.DeleteOutline,
                stringResource(R.string.action_delete_message),
                run(actions.onRetract),
                destructive = true,
            )
        }
        if (moderate) {
            SheetAction(
                Icons.Outlined.DeleteOutline,
                stringResource(R.string.action_moderate_message),
                run(actions.onModerate),
                destructive = true,
            )
        }
        SheetDivider()
        SheetAction(Icons.Outlined.Close, stringResource(R.string.action_cancel), actions.onDismiss)
    }
}

/** Avatar, author, time and the first lines of the message the actions apply to. */
@Composable
private fun MessagePreview(item: TimelineItem) {
    val colors = Waddle.colors
    val clock = rememberConversationClock()
    val author = authorNameOf(item)
    val body = if (item.tombstone != null) {
        stringResource(R.string.message_deleted)
    } else {
        item.body.lineSequence().firstOrNull { it.isNotBlank() }.orEmpty()
    }
    Row(
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = Modifier
            .fillMaxWidth()
            .semantics(mergeDescendants = true) {}
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.xs),
    ) {
        HuddleAvatar(jid = item.authorJid, name = author, size = AvatarSize.SM, ground = colors.surface)
        Column(verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs), modifier = Modifier.weight(1f)) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
            ) {
                Text(
                    text = author,
                    style = AuthorStyle,
                    color = colors.fg,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f, fill = false),
                )
                Text(text = clock.stamp(sentAtOf(item)), style = Waddle.type.time, color = colors.fgMuted, maxLines = 1)
            }
            if (body.isNotEmpty()) {
                Text(
                    text = body,
                    style = MetaStyle.copy(fontSize = Waddle.type.control.fontSize),
                    color = colors.fgMuted,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
    }
}

/**
 * Six 48dp cells: five quick reactions and "more", which opens a wider
 * palette below them in place.
 */
@Composable
private fun QuickReactions(onReact: (String) -> Unit) {
    var more by rememberSaveable { mutableStateOf(false) }
    Column(
        verticalArrangement = Arrangement.spacedBy(Waddle.space.xs),
        modifier = Modifier.padding(horizontal = Waddle.space.md, vertical = Waddle.space.xs),
    ) {
        EmojiRow(QUICK_REACTIONS, onReact) {
            val label = stringResource(R.string.action_more_reactions)
            ReactionCell(
                selected = more,
                modifier = Modifier
                    .weight(1f)
                    .semantics { contentDescription = label },
                onClick = { more = !more },
            ) {
                Icon(
                    Icons.Outlined.AddReaction,
                    contentDescription = null,
                    tint = if (more) Waddle.colors.actionText else Waddle.colors.fgMuted,
                )
            }
        }
        if (more) MORE_REACTIONS.chunked(CELLS).forEach { row -> EmojiRow(row, onReact) }
    }
}

@Composable
private fun EmojiRow(
    emoji: List<String>,
    onReact: (String) -> Unit,
    trailing: (@Composable RowScope.() -> Unit)? = null,
) {
    Row(horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs), modifier = Modifier.fillMaxWidth()) {
        emoji.forEach { glyph ->
            ReactionCell(modifier = Modifier.weight(1f), onClick = { onReact(glyph) }, label = glyph) {
                Text(text = glyph, fontSize = EMOJI_GLYPH)
            }
        }
        trailing?.invoke(this)
    }
}

@Composable
private fun ReactionCell(
    modifier: Modifier,
    onClick: () -> Unit,
    selected: Boolean = false,
    label: String? = null,
    content: @Composable () -> Unit,
) {
    val colors = Waddle.colors
    val shape = RoundedCornerShape(Waddle.radius.control)
    Box(
        contentAlignment = Alignment.Center,
        modifier = modifier
            .height(Waddle.space.xxl)
            .clip(shape)
            .background(if (selected) colors.actionSubtle else colors.surface2, shape)
            .clickable(role = Role.Button, onClickLabel = label, onClick = onClick),
    ) { content() }
}

@Composable
private fun SheetDivider() {
    HorizontalDivider(
        color = Waddle.colors.borderSoft,
        modifier = Modifier.padding(vertical = Waddle.space.xxs),
    )
}

/** A 48dp action row: 20dp icon, label; danger for destructive ones. */
@Composable
private fun SheetAction(
    icon: ImageVector,
    label: String,
    onClick: () -> Unit,
    destructive: Boolean = false,
) {
    val colors = Waddle.colors
    val ink = if (destructive) colors.dangerText else colors.fg
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.md),
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = Waddle.space.xxl)
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = Waddle.space.md),
    ) {
        Icon(
            icon,
            contentDescription = null,
            tint = if (destructive) ink else colors.fgMuted,
            modifier = Modifier.size(ACTION_ICON),
        )
        Text(text = label, style = Waddle.type.body, color = ink)
    }
}

private const val CELLS = 6
private val EMOJI_GLYPH = 26.sp
private val ACTION_ICON = 20.dp
