package social.waddle.android.feature.conversation

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
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
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import coil3.compose.AsyncImage
import social.waddle.android.R
import social.waddle.android.client.FileDisposition
import social.waddle.android.client.mentionSpansIn
import social.waddle.android.client.messageMentionsBareJid
import social.waddle.android.client.store.ReactionGroup
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.TimelineSource
import social.waddle.android.jid.localpartOf
import social.waddle.android.jid.resourcepartOf
import social.waddle.client.ffi.WaddleSharedFile
import java.time.Instant
import java.time.OffsetDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle

/** One timeline row: author, time, body, attachment placeholders. */
@Composable
fun MessageCard(
    row: ConversationRow,
    onRetry: (Long) -> Unit,
    modifier: Modifier = Modifier,
    pinnedIds: Set<String> = emptySet(),
    onLongPress: (TimelineItem) -> Unit = {},
    onToggleReaction: (TimelineItem, String) -> Unit = { _, _ -> },
    resolveQuoted: (String) -> TimelineItem? = { null },
    onQuoteClick: (String) -> Unit = {},
    threadReplyCount: Int = 0,
    onOpenThread: ((TimelineItem) -> Unit)? = null,
    selfBareJid: String? = null,
) {
    when (row) {
        is ConversationRow.Stored -> StoredMessageCard(
            item = row.item,
            isPinned = row.item.identityIds.any { it in pinnedIds },
            onLongPress = onLongPress,
            onToggleReaction = onToggleReaction,
            resolveQuoted = resolveQuoted,
            onQuoteClick = onQuoteClick,
            threadReplyCount = threadReplyCount,
            onOpenThread = onOpenThread,
            selfBareJid = selfBareJid,
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
    resolveQuoted: (String) -> TimelineItem?,
    onQuoteClick: (String) -> Unit,
    threadReplyCount: Int,
    onOpenThread: ((TimelineItem) -> Unit)?,
    selfBareJid: String?,
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
        body = mentionStyledBody(item),
        isMine = item.isMine,
        edited = item.edited,
        pinned = isPinned,
        mentionsSelf = messageMentionsBareJid(
            broadcastMention = item.source.broadcastMention,
            mentionUris = item.source.mentionUris,
            selfBareJid = selfBareJid,
        ),
        onLongPress = { onLongPress(item) },
        modifier = modifier,
        header = {
            item.replyToId?.let { replyToId ->
                QuotedReply(
                    quoted = resolveQuoted(replyToId),
                    fallbackSender = item.replyToSender,
                    onClick = { onQuoteClick(replyToId) },
                )
            }
        },
    ) {
        if (isSticker(item)) {
            AttachmentRow(
                icon = { Icon(Icons.Outlined.Mood, contentDescription = null) },
                label = stringResource(R.string.message_sticker),
            )
        }
        sharedFilesOf(item).forEach { file ->
            SharedFileContent(file)
        }
        if (item.reactions.isNotEmpty()) {
            ReactionChips(
                reactions = item.reactions,
                onToggle = { emoji -> onToggleReaction(item, emoji) },
            )
        }
        if (threadReplyCount > 0 && onOpenThread != null) {
            TextButton(onClick = { onOpenThread(item) }) {
                Text(
                    text = pluralStringResource(
                        R.plurals.thread_replies_chip,
                        threadReplyCount,
                        threadReplyCount,
                    ),
                    style = MaterialTheme.typography.labelMedium,
                )
            }
        }
    }
}

/** Collapsed XEP-0461 quote above the reply body; tap scrolls to it. */
@Composable
private fun QuotedReply(
    quoted: TimelineItem?,
    fallbackSender: String?,
    onClick: () -> Unit,
) {
    Surface(
        shape = RoundedCornerShape(8.dp),
        color = MaterialTheme.colorScheme.surfaceContainerHighest,
        onClick = onClick,
        modifier = Modifier.padding(bottom = 2.dp),
    ) {
        Column(modifier = Modifier.padding(horizontal = 8.dp, vertical = 4.dp)) {
            // Web parity: a MUC occupant JID's display name is its
            // resource (nick) — the localpart is the channel's name.
            val author = quoted?.let { authorOf(it) }
                ?: fallbackSender?.let { resourcepartOf(it) ?: localpartOf(it) }
            author?.let {
                Text(
                    text = it,
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.primary,
                )
            }
            Text(
                text = when {
                    quoted == null -> stringResource(R.string.reply_original_unloaded)
                    quoted.tombstone != null -> stringResource(R.string.message_deleted)
                    else -> quoted.body
                },
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
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
        body = AnnotatedString(message.body),
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
    body: AnnotatedString?,
    isMine: Boolean,
    modifier: Modifier = Modifier,
    edited: Boolean = false,
    pinned: Boolean = false,
    mentionsSelf: Boolean = false,
    onLongPress: (() -> Unit)? = null,
    header: @Composable () -> Unit = {},
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
            // Web parity: a message mentioning the signed-in account gets
            // a subtly tinted bubble. Own messages keep the "mine" color —
            // it already dominates, and losing it would blur authorship.
            color = when {
                isMine -> MaterialTheme.colorScheme.primaryContainer
                mentionsSelf -> MaterialTheme.colorScheme.secondaryContainer
                else -> MaterialTheme.colorScheme.surfaceContainerHigh
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
                header()
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

/**
 * One XEP-0447 attachment: inline images render via Coil with a tap-to-
 * expand viewer (web lightbox parity); everything else is a card that
 * opens the URL externally. Encrypted attachments (XEP-0448) cannot be
 * decrypted without OMEMO — they degrade to a plain card.
 */
@Composable
private fun SharedFileContent(file: WaddleSharedFile) {
    val uriHandler = LocalUriHandler.current
    var viewerOpen by remember { mutableStateOf(false) }
    val isInlineImage = file.encrypted == null &&
        FileDisposition.fromWire(file.disposition) == FileDisposition.INLINE &&
        file.mediaType?.startsWith("image/") == true
    if (isInlineImage) {
        AsyncImage(
            model = file.url,
            contentDescription = file.name ?: stringResource(R.string.message_attachment),
            contentScale = ContentScale.Crop,
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(min = 48.dp, max = 240.dp)
                .clip(RoundedCornerShape(8.dp))
                .clickable { viewerOpen = true },
        )
        if (viewerOpen) {
            MediaViewerDialog(
                url = file.url,
                contentDescription = file.name,
                onDismiss = { viewerOpen = false },
            )
        }
    } else {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            // Guarded: the URL is peer-controlled metadata and openUri
            // throws when no activity handles the scheme.
            modifier = Modifier.clickable {
                runCatching { uriHandler.openUri(file.url) }
            },
        ) {
            Icon(
                Icons.Outlined.AttachFile,
                contentDescription = stringResource(R.string.message_attachment),
            )
            Text(
                text = file.name ?: file.url,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.padding(start = 4.dp),
            )
        }
    }
}

/** Full-screen image viewer (single image; tap anywhere to dismiss). */
@Composable
private fun MediaViewerDialog(
    url: String,
    contentDescription: String?,
    onDismiss: () -> Unit,
) {
    Dialog(
        onDismissRequest = onDismiss,
        properties = DialogProperties(usePlatformDefaultWidth = false),
    ) {
        Box(
            modifier = Modifier
                .fillMaxSize()
                .background(Color.Black.copy(alpha = 0.92f))
                .clickable(onClick = onDismiss),
            contentAlignment = Alignment.Center,
        ) {
            AsyncImage(
                model = url,
                contentDescription = contentDescription,
                contentScale = ContentScale.Fit,
                modifier = Modifier.fillMaxSize(),
            )
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

/**
 * The row body with XEP-0372 mention spans styled (primary, medium
 * weight). Offsets come from a remote sender: `mentionSpansIn` clamps
 * and drops anything out of range, so hostile references never crash.
 */
@Composable
private fun mentionStyledBody(item: TimelineItem): AnnotatedString {
    val spans = mentionSpansIn(
        body = item.body,
        references = item.source.references,
        fallbackStart = item.source.replyFallbackStart,
        fallbackEnd = item.source.replyFallbackEnd,
    )
    if (spans.isEmpty()) return AnnotatedString(item.body)
    val mentionStyle = SpanStyle(
        color = MaterialTheme.colorScheme.primary,
        fontWeight = FontWeight.Medium,
    )
    return buildAnnotatedString {
        append(item.body)
        spans.forEach { span -> addStyle(mentionStyle, span.startIndex, span.endIndex) }
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
