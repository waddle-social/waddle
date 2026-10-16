package social.waddle.android.feature.conversation

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.Chat
import androidx.compose.material.icons.automirrored.outlined.Reply
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material.icons.outlined.Delete
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.PushPin
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import social.waddle.android.R
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.theme.Waddle
import social.waddle.android.theme.consistentTextColor

/** Web QUICK_REACTION_EMOJIS parity (reaction-mode.ts). */
private val QUICK_REACTIONS = listOf("👍", "❤️", "😂", "🎉", "👀")

/**
 * Long-press actions for a timeline row: quick reactions, edit/delete
 * for own messages, copy, pin/unpin in rooms, and XEP-0425 moderation
 * of others' messages for room moderators. [actionable] is false
 * when the row has no usable action target id (e.g. a MUC row without
 * a room-assigned stanza id) — reaction/delete/pin hide, copy stays.
 */
@OptIn(ExperimentalMaterial3Api::class)
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
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        containerColor = MaterialTheme.colorScheme.surfaceContainer,
    ) {
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
 * everyday actions, then destructive ones apart at the bottom.
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
    Column(modifier = Modifier.navigationBarsPadding().padding(bottom = Waddle.space.sm)) {
        MessagePreview(item)
        if (target) {
            QuickReactions(onReact = { emoji -> run { actions.onReact(emoji) }() })
        }
        if (target) {
            SheetAction(Icons.AutoMirrored.Outlined.Reply, stringResource(R.string.action_reply), run(actions.onReply))
        }
        val replyInThread = actions.onReplyInThread
        if (replyInThread != null && live) {
            SheetAction(
                Icons.AutoMirrored.Outlined.Chat,
                stringResource(R.string.action_reply_in_thread),
                run(replyInThread),
            )
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
        if (retract || moderate) {
            HorizontalDivider(
                color = Waddle.colors.hairline,
                modifier = Modifier.padding(vertical = Waddle.space.xs),
            )
        }
        if (retract) {
            SheetAction(
                Icons.Outlined.Delete,
                stringResource(R.string.action_delete_message),
                run(actions.onRetract),
                destructive = true,
            )
        }
        if (moderate) {
            SheetAction(
                Icons.Outlined.Delete,
                stringResource(R.string.action_moderate_message),
                run(actions.onModerate),
                destructive = true,
            )
        }
    }
}

/** Author and the first line of the message the actions apply to. */
@Composable
private fun MessagePreview(item: TimelineItem) {
    val body = item.body.lineSequence().firstOrNull { it.isNotBlank() }.orEmpty()
    Column(
        verticalArrangement = Arrangement.spacedBy(2.dp),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Waddle.space.lg)
            .padding(bottom = Waddle.space.sm),
    ) {
        Text(
            text = authorNameOf(item),
            style = MaterialTheme.typography.labelLarge,
            fontWeight = FontWeight.SemiBold,
            color = consistentTextColor(authorNameOf(item)),
        )
        if (body.isNotEmpty()) {
            Text(
                text = body,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

@Composable
private fun QuickReactions(onReact: (String) -> Unit) {
    Row(
        horizontalArrangement = Arrangement.SpaceBetween,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Waddle.space.lg, vertical = Waddle.space.xs),
    ) {
        QUICK_REACTIONS.forEach { emoji ->
            Box(
                contentAlignment = Alignment.Center,
                modifier = Modifier
                    .size(REACTION_TILE)
                    .clip(CircleShape)
                    .background(MaterialTheme.colorScheme.surfaceContainerHigh)
                    .clickable(onClickLabel = emoji) { onReact(emoji) },
            ) {
                Text(text = emoji, fontSize = 22.sp)
            }
        }
    }
}

@Composable
private fun SheetAction(
    icon: ImageVector,
    label: String,
    onClick: () -> Unit,
    destructive: Boolean = false,
) {
    val tint = if (destructive) Waddle.colors.dangerText else MaterialTheme.colorScheme.onSurface
    ListItem(
        leadingContent = {
            Icon(
                icon,
                contentDescription = null,
                tint = if (destructive) tint else MaterialTheme.colorScheme.onSurfaceVariant,
            )
        },
        headlineContent = { Text(text = label, color = tint) },
        colors = ListItemDefaults.colors(containerColor = Color.Transparent),
        modifier = Modifier
            .clickable(onClick = onClick)
            .padding(horizontal = Waddle.space.xs),
    )
}

private val REACTION_TILE = 48.dp
