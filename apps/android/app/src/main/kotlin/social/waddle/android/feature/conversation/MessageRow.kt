package social.waddle.android.feature.conversation

import android.content.Context
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
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.InsertDriveFile
import androidx.compose.material.icons.automirrored.outlined.Reply
import androidx.compose.material.icons.outlined.ChatBubbleOutline
import androidx.compose.material.icons.outlined.DeleteOutline
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.Mood
import androidx.compose.material.icons.outlined.PushPin
import androidx.compose.material.icons.outlined.Schedule
import androidx.compose.material3.Icon
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.platform.UriHandler
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import coil3.compose.AsyncImage
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import social.waddle.android.R
import social.waddle.android.WaddleApplication
import social.waddle.android.avatar.PeerAvatar
import social.waddle.android.client.AuthorBadge
import social.waddle.android.client.AuthorBadgeKind
import social.waddle.android.client.FileDisposition
import social.waddle.android.client.isImageUrl
import social.waddle.android.client.messageAuthorBadgeOf
import social.waddle.android.client.richBlocksOf
import social.waddle.android.client.store.ReactionGroup
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.jid.localpartOf
import social.waddle.android.jid.resourcepartOf
import social.waddle.android.media.imageModelOf
import social.waddle.android.media.isInlineImage
import social.waddle.android.theme.MonoMeta
import social.waddle.android.theme.Waddle
import social.waddle.android.theme.consistentTextColor
import social.waddle.client.ffi.WaddlePresence
import social.waddle.client.ffi.WaddleSharedFile
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle

/** Everything a timeline row can do, bundled so rows take one parameter. */
class MessageRowActions(
    val onLongPress: (TimelineItem) -> Unit = {},
    val onToggleReaction: (TimelineItem, String) -> Unit = { _, _ -> },
    val onOpenThread: ((TimelineItem) -> Unit)? = null,
    val onQuoteClick: (TimelineItem) -> Unit = {},
    val onRetry: (Long) -> Unit = {},
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
 * One feed row, Slack/web style: a 36dp avatar gutter, a header (name,
 * badge, time) on the first message of an author burst, then the body.
 * Every author — the reader included — sits on the same left edge.
 */
@Composable
fun MessageRow(
    entry: FeedEntry,
    actions: MessageRowActions,
    context: MessageRowContext,
    modifier: Modifier = Modifier,
) {
    when (val row = entry.row) {
        is ConversationRow.Stored -> StoredMessageRow(entry, row.item, actions, context, modifier)
        is ConversationRow.Unconfirmed -> PendingMessageRow(entry, row.message, actions, modifier)
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun RowFrame(
    entry: FeedEntry,
    author: String,
    authorJid: String?,
    onLongPress: (() -> Unit)?,
    modifier: Modifier = Modifier,
    header: @Composable () -> Unit,
    content: @Composable () -> Unit,
) {
    val haptics = LocalHapticFeedback.current
    val colors = Waddle.colors
    val mention = entry.mentionsSelf
    Row(
        modifier = modifier
            .fillMaxWidth()
            .then(
                if (mention) {
                    Modifier.drawBehind {
                        drawRect(colors.mentionTint)
                        drawRect(colors.live, size = size.copy(width = MENTION_BAR.toPx()))
                    }
                } else {
                    Modifier
                },
            )
            .then(
                if (onLongPress != null) {
                    Modifier.combinedClickable(
                        onClick = {},
                        onLongClick = {
                            haptics.performHapticFeedback(HapticFeedbackType.LongPress)
                            onLongPress()
                        },
                    )
                } else {
                    Modifier
                },
            )
            .semantics(mergeDescendants = true) {}
            .padding(
                start = Waddle.space.md,
                end = Waddle.space.md,
                top = if (entry.startsGroup) Waddle.space.sm else ROW_GAP,
                bottom = ROW_GAP,
            ),
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
    ) {
        Box(modifier = Modifier.width(AVATAR_SIZE)) {
            if (entry.startsGroup) {
                PeerAvatar(jid = authorJid, displayName = author, size = AVATAR_SIZE)
            }
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

@Composable
private fun StoredMessageRow(
    entry: FeedEntry,
    item: TimelineItem,
    actions: MessageRowActions,
    context: MessageRowContext,
    modifier: Modifier,
) {
    val author = authorNameOf(item)
    RowFrame(
        entry = entry,
        author = author,
        authorJid = entry.authorJid,
        onLongPress = if (item.rejected) null else ({ actions.onLongPress(item) }),
        modifier = modifier,
        header = {
            MessageHeader(
                author = author,
                badge = if (isGroupchatRow(item)) {
                    messageAuthorBadgeOf(item.authorJid, context.declaredBotJids) { context.authorPresence[author] }
                } else {
                    null
                },
                sentAt = entry.sentAt,
                edited = item.edited && item.tombstone == null,
                pinned = entry.isPinned,
            )
        },
    ) {
        item.replyToId?.let {
            ReplyPreview(
                parent = entry.replyParent,
                fallbackSender = item.replyToSender,
                authorJid = entry.replyParentAuthorJid,
                onClick = { entry.replyParent?.let(actions.onQuoteClick) },
            )
        }
        if (item.tombstone != null) {
            TombstoneLine()
            return@RowFrame
        }
        MessageContent(item, context)
        // A continuation row has no header to carry the mark.
        if (item.edited && !entry.startsGroup) {
            Text(
                text = stringResource(R.string.message_edited),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        if (item.reactions.isNotEmpty()) {
            ReactionPills(item.reactions) { emoji -> actions.onToggleReaction(item, emoji) }
        }
        item.safetyScores?.let { scores ->
            val category = safetyScoreChipCategory(scores) ?: return@let
            var scoresOpen by remember { mutableStateOf(false) }
            SafetyScoresChip(category = category, onClick = { scoresOpen = true })
            if (scoresOpen) SafetyScoresSheet(scores = scores, onDismiss = { scoresOpen = false })
        }
        val openThread = actions.onOpenThread
        if (entry.replyCount > 0 && openThread != null) {
            ThreadChip(count = entry.replyCount, onClick = { openThread(item) })
        }
        if (item.rejected) {
            DeliveryLine(
                icon = Icons.Outlined.ErrorOutline,
                text = stringResource(R.string.message_delivery_failed),
                color = Waddle.colors.dangerText,
            )
        }
    }
}

@Composable
private fun PendingMessageRow(
    entry: FeedEntry,
    message: PendingMessage,
    actions: MessageRowActions,
    modifier: Modifier,
) {
    val author = stringResource(R.string.message_author_me)
    RowFrame(
        entry = entry,
        author = author,
        authorJid = entry.authorJid,
        onLongPress = null,
        modifier = modifier,
        header = {
            MessageHeader(author = author, badge = null, sentAt = entry.sentAt, edited = false, pinned = false)
        },
    ) {
        // Dimmed until the server has it: the row is real but not yet shared.
        Text(
            text = message.body,
            style = MaterialTheme.typography.bodyLarge,
            modifier = Modifier.alpha(if (message.acked) 1f else PENDING_ALPHA),
        )
        when {
            message.failed -> Row(verticalAlignment = Alignment.CenterVertically) {
                DeliveryLine(
                    icon = Icons.Outlined.ErrorOutline,
                    text = stringResource(R.string.message_delivery_failed),
                    color = Waddle.colors.dangerText,
                )
                Text(
                    text = stringResource(R.string.message_retry),
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier
                        .clip(RoundedCornerShape(Waddle.radius.tag))
                        .clickable { actions.onRetry(message.localId) }
                        .padding(horizontal = Waddle.space.xs, vertical = Waddle.space.xxs),
                )
            }
            message.queued -> DeliveryLine(
                icon = Icons.Outlined.Schedule,
                text = stringResource(R.string.message_queued),
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            !message.acked -> DeliveryLine(
                icon = null,
                text = stringResource(R.string.message_sending),
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

/** Name · badge · time · (edited) · pin. */
@Composable
private fun MessageHeader(
    author: String,
    badge: AuthorBadge?,
    sentAt: Instant,
    edited: Boolean,
    pinned: Boolean,
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
    ) {
        Text(
            text = author,
            style = MaterialTheme.typography.labelLarge,
            fontWeight = FontWeight.SemiBold,
            color = consistentTextColor(author),
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f, fill = false),
        )
        badge?.let { BadgeLabel(it) }
        Text(
            text = formatTime(sentAt),
            style = MonoMeta,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
        )
        if (edited) {
            Text(
                text = stringResource(R.string.message_edited),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        if (pinned) {
            Icon(
                Icons.Outlined.PushPin,
                contentDescription = stringResource(R.string.message_pinned),
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.size(12.dp),
            )
        }
    }
}

/** Quiet uppercase annotation beside the name, never a filled chip (web `.chat-hat-tag`). */
@Composable
private fun BadgeLabel(badge: AuthorBadge) {
    Text(
        text = badge.label.uppercase(),
        style = MaterialTheme.typography.labelSmall.copy(
            fontSize = 10.sp,
            letterSpacing = 0.06.em,
            fontWeight = FontWeight.SemiBold,
        ),
        color = badgeColor(badge.kind),
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
        modifier = Modifier.widthIn(max = 120.dp),
    )
}

@Composable
private fun badgeColor(kind: AuthorBadgeKind): Color = when (kind) {
    AuthorBadgeKind.OWNER -> Waddle.colors.goldText
    AuthorBadgeKind.ADMIN, AuthorBadgeKind.MODERATOR, AuthorBadgeKind.VERIFIED -> MaterialTheme.colorScheme.primary
    AuthorBadgeKind.BOT, AuthorBadgeKind.HAT -> MaterialTheme.colorScheme.onSurfaceVariant
}

/** Body, media, files and link cards, in web `displayBody` precedence. */
@Composable
private fun MessageContent(item: TimelineItem, context: MessageRowContext) {
    val stickerFile = stickerFileOf(item)
    val inlineImageUrl = item.body.trim().takeIf {
        stickerFile == null && item.sharedFiles.isEmpty() && isImageUrl(item.body)
    }
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
    when {
        stickerFile != null -> StickerContent(stickerFile, altText = item.body)
        else -> {
            if (item.isSticker) {
                FileCard(
                    icon = { Icon(Icons.Outlined.Mood, contentDescription = null) },
                    title = stringResource(R.string.message_sticker),
                    subtitle = null,
                    onClick = null,
                )
            }
            inlineImageUrl?.let { url -> SharedFileContent(inlineImageFileOf(url)) }
            item.sharedFiles.forEach { file -> SharedFileContent(file) }
        }
    }
    item.linkPreviews.forEach { preview -> LinkPreviewCard(preview, context.trustedMediaOrigin) }
}

@Composable
private fun TombstoneLine() {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Icon(
            Icons.Outlined.DeleteOutline,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.size(16.dp),
        )
        Text(
            text = stringResource(R.string.message_deleted),
            style = MaterialTheme.typography.bodyMedium,
            fontStyle = FontStyle.Italic,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(start = Waddle.space.xxs),
        )
    }
}

@Composable
private fun DeliveryLine(icon: ImageVector?, text: String, color: Color) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        icon?.let {
            Icon(it, contentDescription = null, tint = color, modifier = Modifier.size(14.dp))
        }
        Text(
            text = text,
            style = MaterialTheme.typography.labelMedium,
            color = color,
            modifier = Modifier.padding(start = if (icon != null) Waddle.space.xxs else 0.dp),
        )
    }
}

/** XEP-0461 quote: accent rule, author, one line of the parent. Tap jumps to it. */
@Composable
private fun ReplyPreview(
    parent: TimelineItem?,
    fallbackSender: String?,
    authorJid: String?,
    onClick: () -> Unit,
) {
    // A MUC occupant JID's display name is its resource (nick).
    val author = parent?.let { authorNameOf(it) }
        ?: fallbackSender?.let { resourcepartOf(it) ?: localpartOf(it) }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
        modifier = Modifier
            .clip(RoundedCornerShape(Waddle.radius.tag))
            .clickable(enabled = parent != null, onClick = onClick)
            .padding(vertical = 2.dp),
    ) {
        Icon(
            Icons.AutoMirrored.Outlined.Reply,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.size(14.dp),
        )
        author?.let {
            PeerAvatar(jid = authorJid, displayName = it, size = QUOTE_AVATAR_SIZE)
            Text(
                text = it,
                style = MaterialTheme.typography.labelMedium,
                fontWeight = FontWeight.SemiBold,
                color = MaterialTheme.colorScheme.onSurface,
                maxLines = 1,
            )
        }
        Text(
            text = when {
                parent == null -> stringResource(R.string.reply_original_unloaded)
                parent.tombstone != null -> stringResource(R.string.message_deleted)
                else -> parent.body
            },
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/** XEP-0444 aggregation as pills; tap toggles the reader's reaction. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun ReactionPills(reactions: List<ReactionGroup>, onToggle: (String) -> Unit) {
    val haptics = LocalHapticFeedback.current
    FlowRow(
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
        verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
        modifier = Modifier.padding(top = 2.dp),
    ) {
        reactions.forEach { group ->
            val primary = MaterialTheme.colorScheme.primary
            Surface(
                shape = CircleShape,
                color = if (group.mine) primary.copy(alpha = 0.14f) else MaterialTheme.colorScheme.surfaceContainerHigh,
                border = BorderStroke(
                    1.dp,
                    if (group.mine) primary.copy(alpha = 0.6f) else MaterialTheme.colorScheme.outlineVariant,
                ),
                onClick = {
                    haptics.performHapticFeedback(HapticFeedbackType.TextHandleMove)
                    onToggle(group.emoji)
                },
            ) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
                    modifier = Modifier.padding(horizontal = 10.dp, vertical = 3.dp),
                ) {
                    Text(text = group.emoji, style = MaterialTheme.typography.bodyMedium)
                    if (group.count > 1) {
                        Text(
                            text = group.count.toString(),
                            style = MonoMeta,
                            color = if (group.mine) primary else MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
        }
    }
}

/** "N replies" link under a thread root (web `.chat-thread-chip`). */
@Composable
private fun ThreadChip(count: Int, onClick: () -> Unit) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
        modifier = Modifier
            .padding(top = 2.dp)
            .clip(RoundedCornerShape(Waddle.radius.tag))
            .clickable(onClick = onClick)
            .padding(vertical = 2.dp, horizontal = 2.dp),
    ) {
        Icon(
            Icons.Outlined.ChatBubbleOutline,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.primary,
            modifier = Modifier.size(14.dp),
        )
        Text(
            text = pluralStringResource(R.plurals.thread_replies_chip, count, count),
            style = MaterialTheme.typography.labelMedium,
            fontWeight = FontWeight.SemiBold,
            color = MaterialTheme.colorScheme.primary,
        )
    }
}

/**
 * One XEP-0447 attachment. Images keep their real aspect ratio (from the
 * file metadata when present) inside a 280dp column and open a full-screen
 * viewer; XEP-0448 encrypted images decrypt through the registered Coil
 * fetcher via [imageModelOf]. Everything else is a file card.
 */
@Composable
private fun SharedFileContent(file: WaddleSharedFile) {
    val uriHandler = LocalUriHandler.current
    var viewerOpen by remember { mutableStateOf(false) }
    if (isInlineImage(file)) {
        val ratio = aspectRatioOf(file)
        AsyncImage(
            model = imageModelOf(file),
            contentDescription = file.name ?: stringResource(R.string.message_attachment),
            contentScale = ContentScale.Crop,
            modifier = Modifier
                .padding(top = 2.dp)
                .widthIn(max = MEDIA_MAX_WIDTH)
                .fillMaxWidth()
                .aspectRatio(ratio)
                .clip(RoundedCornerShape(Waddle.radius.control))
                .background(MaterialTheme.colorScheme.surfaceContainerHigh)
                .clickable { viewerOpen = true },
        )
        if (viewerOpen) {
            MediaViewerDialog(model = imageModelOf(file), contentDescription = file.name) { viewerOpen = false }
        }
        return
    }
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var openFailed by remember(file.url) { mutableStateOf(false) }
    FileCard(
        icon = {
            Icon(
                if (file.encrypted != null) Icons.Outlined.Lock else Icons.AutoMirrored.Outlined.InsertDriveFile,
                contentDescription = null,
            )
        },
        title = file.name ?: file.url.substringAfterLast('/').ifBlank { file.url },
        subtitle = fileSubtitleOf(file),
        onClick = {
            openFailed = false
            openSharedFile(context, uriHandler, scope, file) { openFailed = true }
        },
    )
    if (openFailed) {
        Text(
            text = stringResource(R.string.attachment_open_failed),
            style = MaterialTheme.typography.labelSmall,
            color = Waddle.colors.dangerText,
        )
    }
}

@Composable
private fun FileCard(
    icon: @Composable () -> Unit,
    title: String,
    subtitle: String?,
    onClick: (() -> Unit)?,
) {
    Surface(
        shape = RoundedCornerShape(Waddle.radius.control),
        color = MaterialTheme.colorScheme.surfaceContainerLow,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
        modifier = Modifier
            .padding(top = 2.dp)
            .widthIn(max = FILE_CARD_MAX_WIDTH)
            .fillMaxWidth()
            .then(if (onClick != null) Modifier.clickable(onClick = onClick) else Modifier),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
            modifier = Modifier.padding(Waddle.space.sm),
        ) {
            Box(
                contentAlignment = Alignment.Center,
                modifier = Modifier
                    .size(36.dp)
                    .clip(RoundedCornerShape(Waddle.radius.control))
                    .background(MaterialTheme.colorScheme.primary.copy(alpha = 0.12f)),
            ) {
                CompositionLocalProvider(LocalContentColor provides MaterialTheme.colorScheme.primary) { icon() }
            }
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = title,
                    style = MaterialTheme.typography.labelLarge,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                subtitle?.let {
                    Text(
                        text = it,
                        style = MonoMeta,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                    )
                }
            }
        }
    }
}

/**
 * Open a tapped non-image attachment. Plaintext files hand the URL to the
 * system (guarded: the URL is peer-controlled and openUri throws when no
 * activity handles the scheme). XEP-0448 files would show an external
 * viewer only ciphertext, so they route through the download → verify →
 * decrypt → FileProvider opener instead.
 */
private fun openSharedFile(
    context: Context,
    uriHandler: UriHandler,
    scope: CoroutineScope,
    file: WaddleSharedFile,
    onFailure: () -> Unit,
) {
    if (file.encrypted == null) {
        runCatching { uriHandler.openUri(file.url) }.onFailure { onFailure() }
        return
    }
    val opener = (context.applicationContext as? WaddleApplication)?.graph?.encryptedAttachmentOpener
    if (opener == null) {
        onFailure()
        return
    }
    scope.launch { if (!opener.open(file)) onFailure() }
}

/** Full-screen image viewer (tap anywhere to dismiss). */
@Composable
private fun MediaViewerDialog(model: Any, contentDescription: String?, onDismiss: () -> Unit) {
    Dialog(onDismissRequest = onDismiss, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Box(
            modifier = Modifier
                .fillMaxSize()
                .background(Color.Black.copy(alpha = 0.94f))
                .clickable(onClick = onDismiss),
            contentAlignment = Alignment.Center,
        ) {
            AsyncImage(
                model = model,
                contentDescription = contentDescription,
                contentScale = ContentScale.Fit,
                modifier = Modifier.fillMaxSize(),
            )
        }
    }
}

/** XEP-0449 sticker: compact inline image, alt-text is the body (web: 112px). */
@Composable
private fun StickerContent(file: WaddleSharedFile, altText: String) {
    var viewerOpen by remember { mutableStateOf(false) }
    AsyncImage(
        model = imageModelOf(file),
        contentDescription = altText.ifBlank { stringResource(R.string.message_sticker) },
        contentScale = ContentScale.Fit,
        modifier = Modifier
            .size(STICKER_SIZE)
            .clip(RoundedCornerShape(Waddle.radius.control))
            .clickable { viewerOpen = true },
    )
    if (viewerOpen) {
        MediaViewerDialog(model = imageModelOf(file), contentDescription = altText) { viewerOpen = false }
    }
}

/** A lone image-URL body (the GIF send shape) rendered as an inline image. */
private fun inlineImageFileOf(url: String) = WaddleSharedFile(
    url = url,
    name = null,
    mediaType = INLINE_IMAGE_MEDIA_TYPE,
    size = null,
    width = null,
    height = null,
    desc = null,
    hashes = emptyList(),
    disposition = FileDisposition.INLINE.wire,
    encrypted = null,
)

/** The XEP-0447 file a sticker renders as: its first inline image. */
private fun stickerFileOf(item: TimelineItem): WaddleSharedFile? {
    if (!item.isSticker) return null
    return item.sharedFiles.firstOrNull(::isInlineImage)
}

/** Declared aspect ratio, clamped so panoramas and tall shots stay sane. */
private fun aspectRatioOf(file: WaddleSharedFile): Float {
    val w = file.width?.toFloat() ?: return DEFAULT_MEDIA_RATIO
    val h = file.height?.toFloat() ?: return DEFAULT_MEDIA_RATIO
    if (w <= 0f || h <= 0f) return DEFAULT_MEDIA_RATIO
    return (w / h).coerceIn(MIN_MEDIA_RATIO, MAX_MEDIA_RATIO)
}

private fun fileSubtitleOf(file: WaddleSharedFile): String? {
    val size = file.size?.toLong()?.let(::humanSize)
    val type = file.mediaType?.substringAfter('/')?.uppercase()?.takeIf { it.length <= MAX_TYPE_LABEL }
    return listOfNotNull(type, size).joinToString(" · ").ifBlank { null }
}

private fun humanSize(bytes: Long): String = when {
    bytes < KIB -> "$bytes B"
    bytes < KIB * KIB -> "%.0f KB".format(bytes / KIB.toDouble())
    else -> "%.1f MB".format(bytes / (KIB * KIB).toDouble())
}

/** Display name: the occupant nick in a MUC, the localpart in 1:1. */
@Composable
internal fun authorNameOf(item: TimelineItem): String {
    val from = item.from ?: return stringResource(R.string.message_unknown_sender)
    return if (isGroupchatRow(item)) resourcepartOf(from) ?: localpartOf(from) else localpartOf(from)
}

private val TIME_FORMAT: DateTimeFormatter = DateTimeFormatter.ofLocalizedTime(FormatStyle.SHORT)

internal fun formatTime(instant: Instant, zone: ZoneId = ZoneId.systemDefault()): String =
    TIME_FORMAT.withZone(zone).format(instant)

private val AVATAR_SIZE: Dp = 36.dp
private val QUOTE_AVATAR_SIZE: Dp = 16.dp
private val ROW_GAP: Dp = 2.dp
private val MENTION_BAR: Dp = 3.dp
private val MEDIA_MAX_WIDTH: Dp = 280.dp
private val FILE_CARD_MAX_WIDTH: Dp = 320.dp
private val STICKER_SIZE: Dp = 112.dp
private const val PENDING_ALPHA = 0.72f
private const val DEFAULT_MEDIA_RATIO = 4f / 3f
private const val MIN_MEDIA_RATIO = 0.6f
private const val MAX_MEDIA_RATIO = 2.2f
private const val MAX_TYPE_LABEL = 6
private const val KIB = 1024L
private const val INLINE_IMAGE_MEDIA_TYPE = "image/gif"
