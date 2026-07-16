package social.waddle.android.feature.conversation

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.Chat
import androidx.compose.material.icons.automirrored.outlined.Reply
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material.icons.outlined.Delete
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.PushPin
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import social.waddle.android.R
import social.waddle.android.client.store.TimelineItem

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
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(modifier = Modifier.navigationBarsPadding().padding(bottom = 8.dp)) {
            if (actionable && item.tombstone == null) {
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 16.dp, vertical = 4.dp),
                    horizontalArrangement = Arrangement.SpaceEvenly,
                ) {
                    QUICK_REACTIONS.forEach { emoji ->
                        TextButton(onClick = {
                            onReact(emoji)
                        onDismiss()
                        }) {
                            Text(text = emoji, style = MaterialTheme.typography.headlineSmall)
                        }
                    }
                }
            }
            if (actionable && item.tombstone == null) {
                SheetAction(
                    icon = { Icon(Icons.AutoMirrored.Outlined.Reply, contentDescription = null) },
                    label = stringResource(R.string.action_reply),
                ) {
                    onReply()
                    onDismiss()
                }
            }
            if (onReplyInThread != null && item.tombstone == null) {
                SheetAction(
                    icon = { Icon(Icons.AutoMirrored.Outlined.Chat, contentDescription = null) },
                    label = stringResource(R.string.action_reply_in_thread),
                ) {
                    onReplyInThread()
                    onDismiss()
                }
            }
            if (item.isMine && actionable && item.tombstone == null) {
                SheetAction(
                    icon = { Icon(Icons.Outlined.Edit, contentDescription = null) },
                    label = stringResource(R.string.action_edit_message),
                ) {
                    onEdit()
                    onDismiss()
                }
                SheetAction(
                    icon = {
                        Icon(
                            Icons.Outlined.Delete,
                            contentDescription = null,
                            tint = MaterialTheme.colorScheme.error,
                        )
                    },
                    label = stringResource(R.string.action_delete_message),
                    labelColor = MaterialTheme.colorScheme.error,
                ) {
                    onRetract()
                    onDismiss()
                }
            }
            if (item.tombstone == null) {
                SheetAction(
                    icon = { Icon(Icons.Outlined.ContentCopy, contentDescription = null) },
                    label = stringResource(R.string.action_copy_message),
                ) {
                    onCopy()
                    onDismiss()
                }
            }
            if (canPin && actionable && item.tombstone == null) {
                SheetAction(
                    icon = { Icon(Icons.Outlined.PushPin, contentDescription = null) },
                    label = stringResource(
                        if (isPinned) R.string.action_unpin_message else R.string.action_pin_message,
                    ),
                ) {
                    onSetPinned(!isPinned)
                    onDismiss()
                }
            }
            // XEP-0425: moderators remove OTHERS' messages; own rows
            // use the retract action above instead.
            if (canModerate && !item.isMine && actionable && item.tombstone == null) {
                SheetAction(
                    icon = {
                        Icon(
                            Icons.Outlined.Delete,
                            contentDescription = null,
                            tint = MaterialTheme.colorScheme.error,
                        )
                    },
                    label = stringResource(R.string.action_moderate_message),
                    labelColor = MaterialTheme.colorScheme.error,
                ) {
                    onModerate()
                    onDismiss()
                }
            }
        }
    }
}

@Composable
private fun SheetAction(
    icon: @Composable () -> Unit,
    label: String,
    labelColor: Color = Color.Unspecified,
    onClick: () -> Unit,
) {
    ListItem(
        leadingContent = icon,
        headlineContent = { Text(text = label, color = labelColor) },
        colors = ListItemDefaults.colors(containerColor = Color.Transparent),
        modifier = Modifier.clickable(onClick = onClick),
    )
}
