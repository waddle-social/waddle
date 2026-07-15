package social.waddle.android.feature.conversation

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.AttachFile
import androidx.compose.material.icons.outlined.DeleteOutline
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.Mood
import androidx.compose.material.icons.outlined.PushPin
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import social.waddle.android.client.store.ReactionGroup
import java.time.Instant
import java.time.OffsetDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import social.waddle.android.R
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.TimelineSource
import social.waddle.android.jid.localpartOf
import social.waddle.android.jid.resourcepartOf
import social.waddle.client.ffi.WaddleSharedFile

/** One timeline row: author, time, body, attachment placeholders. */
@Composable
fun MessageCard(
    row: ConversationRow,
    onRetry: (Long) -> Unit,
    modifier: Modifier = Modifier,
    pinnedIds: Set<String> = emptySet(),
    onLongPress: (TimelineItem) -> Unit = {},
    onToggleReaction: (TimelineItem, String) -> Unit = { _, _ -> },
) {
    when (row) {
        is ConversationRow.Stored -> StoredMessageCard(
            item = row.item,
            isPinned = row.item.stanzaId?.let { it in pinnedIds } == true,
            onLongPress = onLongPress,
            onToggleReaction = onToggleReaction,
            modifier = modifier,
        )
        is ConversationRow.Unconfirmed -> PendingMessageCard(
            message = row.message,
            onRetry = onRetry,
            modifier = modifier,
        )
    }
}

@Composable
private fun StoredMessageCard(
    item: TimelineItem,
    isPinned: Boolean,
    onLongPress: (TimelineItem) -> Unit,
    onToggleReaction: (TimelineItem, String) -> Unit,
    modifier: Modifier = Modifier,
) {
    if (item.tombstone != null) {
        MessageBubble(
            author = authorOf(item),
            time = formatTimestamp(item.timestamp),
            body = null,
            isMine = item.isMine,
            modifier = modifier,
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    Icons.Outlined.DeleteOutline,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Text(
                    text = stringResource(R.string.message_deleted),
                    style = MaterialTheme.typography.bodyMedium,
                    fontStyle = FontStyle.Italic,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(start = 4.dp),
                )
            }
        }
        return
    }
    MessageBubble(
        author = authorOf(item),
        time = formatTimestamp(item.timestamp),
        body = item.body,
        isMine = item.isMine,
        edited = item.edited,
        pinned = isPinned,
        onLongPress = { onLongPress(item) },
        modifier = modifier,
    ) {
        if (isSticker(item)) {
            AttachmentRow(
                icon = { Icon(Icons.Outlined.Mood, contentDescription = null) },
                label = stringResource(R.string.message_sticker),
            )
        }
        sharedFilesOf(item).forEach { file ->
            AttachmentRow(
                icon = {
                    Icon(
                        Icons.Outlined.AttachFile,
                        contentDescription = stringResource(R.string.message_attachment),
                    )
                },
                label = file.name ?: file.url,
            )
        }
        if (item.reactions.isNotEmpty()) {
            ReactionChips(
                reactions = item.reactions,
                onToggle = { emoji -> onToggleReaction(item, emoji) },
            )
        }
    }
}

@Composable
private fun PendingMessageCard(
    message: PendingMessage,
    onRetry: (Long) -> Unit,
    modifier: Modifier = Modifier,
) {
    MessageBubble(
        author = stringResource(R.string.message_author_me),
        time = when {
            message.failed -> null
            message.queued -> stringResource(R.string.message_queued)
            else -> stringResource(R.string.message_sending)
        },
        body = message.body,
        isMine = true,
        modifier = modifier,
    ) {
        if (message.failed) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    Icons.Outlined.ErrorOutline,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.error,
                )
                Text(
                    text = stringResource(R.string.message_delivery_failed),
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.padding(start = 4.dp),
                )
                TextButton(onClick = { onRetry(message.localId) }) {
                    Text(text = stringResource(R.string.message_retry))
                }
            }
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun MessageBubble(
    author: String,
    time: String?,
    body: String?,
    isMine: Boolean,
    modifier: Modifier = Modifier,
    edited: Boolean = false,
    pinned: Boolean = false,
    onLongPress: (() -> Unit)? = null,
    extras: @Composable () -> Unit,
) {
    Column(
        modifier = modifier
            .fillMaxWidth()
            .padding(horizontal = 12.dp, vertical = 3.dp),
        horizontalAlignment = if (isMine) Alignment.End else Alignment.Start,
    ) {
        Surface(
            shape = RoundedCornerShape(12.dp),
            color = if (isMine) {
                MaterialTheme.colorScheme.primaryContainer
            } else {
                MaterialTheme.colorScheme.surfaceContainerHigh
            },
            modifier = Modifier
                .widthIn(max = 340.dp)
                .semantics(mergeDescendants = true) {}
                .then(
                    if (onLongPress != null) {
                        Modifier.combinedClickable(onClick = {}, onLongClick = onLongPress)
                    } else {
                        Modifier
                    },
                ),
        ) {
            Column(
                modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
                verticalArrangement = Arrangement.spacedBy(2.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        text = author,
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.primary,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.padding(end = 8.dp),
                    )
                    time?.let {
                        Text(
                            text = it,
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    if (edited) {
                        Text(
                            text = stringResource(R.string.message_edited),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.padding(start = 4.dp),
                        )
                    }
                    if (pinned) {
                        Icon(
                            Icons.Outlined.PushPin,
                            contentDescription = stringResource(R.string.message_pinned),
                            tint = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.padding(start = 4.dp).size(14.dp),
                        )
                    }
                }
                body?.let { Text(text = it, style = MaterialTheme.typography.bodyLarge) }
                extras()
            }
        }
    }
}

/** XEP-0444 aggregation chips; tap toggles the account's reaction. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun ReactionChips(
    reactions: List<ReactionGroup>,
    onToggle: (String) -> Unit,
) {
    FlowRow(
        horizontalArrangement = Arrangement.spacedBy(4.dp),
        modifier = Modifier.padding(top = 2.dp),
    ) {
        reactions.forEach { group ->
            Surface(
                shape = RoundedCornerShape(10.dp),
                color = if (group.mine) {
                    MaterialTheme.colorScheme.primary.copy(alpha = 0.18f)
                } else {
                    MaterialTheme.colorScheme.surfaceContainerHighest
                },
                border = if (group.mine) {
                    BorderStroke(1.dp, MaterialTheme.colorScheme.primary)
                } else {
                    null
                },
                onClick = { onToggle(group.emoji) },
            ) {
                Text(
                    text = if (group.count > 1) "${group.emoji} ${group.count}" else group.emoji,
                    style = MaterialTheme.typography.labelMedium,
                    modifier = Modifier.padding(horizontal = 8.dp, vertical = 3.dp),
                )
            }
        }
    }
}

@Composable
private fun AttachmentRow(icon: @Composable () -> Unit, label: String) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        icon()
        Text(
            text = label,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.padding(start = 4.dp),
        )
    }
}

@Composable
private fun authorOf(item: TimelineItem): String {
    val from = item.from ?: return stringResource(R.string.message_unknown_sender)
    return if (isGroupchat(item)) {
        resourcepartOf(from) ?: localpartOf(from)
    } else {
        localpartOf(from)
    }
}

private fun isGroupchat(item: TimelineItem): Boolean = when (val source = item.source) {
    is TimelineSource.Live ->
        source.message.isMuc || source.message.messageType == MESSAGE_TYPE_GROUPCHAT
    is TimelineSource.Archived -> source.message.messageType == MESSAGE_TYPE_GROUPCHAT
}

private fun sharedFilesOf(item: TimelineItem): List<WaddleSharedFile> =
    when (val source = item.source) {
        is TimelineSource.Live -> source.message.sharedFiles
        is TimelineSource.Archived -> source.message.sharedFiles
    }

private fun isSticker(item: TimelineItem): Boolean = when (val source = item.source) {
    is TimelineSource.Live -> source.message.isSticker
    is TimelineSource.Archived -> source.message.isSticker
}

private val TIME_FORMAT: DateTimeFormatter =
    DateTimeFormatter.ofLocalizedTime(FormatStyle.SHORT)

private fun formatTimestamp(timestamp: String?): String? {
    timestamp ?: return null
    val instant = runCatching { Instant.parse(timestamp) }.getOrElse {
        runCatching { OffsetDateTime.parse(timestamp).toInstant() }.getOrNull()
    } ?: return null
    return TIME_FORMAT.withZone(ZoneId.systemDefault()).format(instant)
}

private const val MESSAGE_TYPE_GROUPCHAT = "groupchat"
