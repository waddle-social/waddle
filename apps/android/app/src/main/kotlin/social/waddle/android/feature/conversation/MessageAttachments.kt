package social.waddle.android.feature.conversation

import android.content.Context
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.systemBarsPadding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.InsertDriveFile
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.Mood
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.platform.UriHandler
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import coil3.compose.AsyncImage
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import social.waddle.android.R
import social.waddle.android.WaddleApplication
import social.waddle.android.client.FileDisposition
import social.waddle.android.client.isImageUrl
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.media.imageModelOf
import social.waddle.android.media.isInlineImage
import social.waddle.android.theme.Waddle
import social.waddle.client.ffi.WaddleSharedFile

/** Stickers, inline images and file cards of one message. */
@Composable
internal fun MessageAttachments(item: TimelineItem, stickerFile: WaddleSharedFile?, inlineImageUrl: String?) {
    when {
        stickerFile != null -> StickerContent(stickerFile, altText = item.body)
        else -> {
            if (item.isSticker) {
                FileCard(
                    icon = Icons.Outlined.Mood,
                    title = stringResource(R.string.message_sticker),
                    subtitle = null,
                    onClick = null,
                )
            }
            inlineImageUrl?.let { url -> SharedFileContent(inlineImageFileOf(url)) }
            item.sharedFiles.forEach { file -> SharedFileContent(file) }
        }
    }
}

/**
 * One XEP-0447 attachment. Images keep their real aspect ratio (from the
 * file metadata when present) inside a 320×256dp box with a hairline
 * edge, and open a full-screen viewer; XEP-0448 encrypted images decrypt
 * through the registered Coil fetcher via [imageModelOf]. Everything
 * else is a file card.
 */
@Composable
private fun SharedFileContent(file: WaddleSharedFile) {
    val uriHandler = LocalUriHandler.current
    var viewerOpen by remember { mutableStateOf(false) }
    if (isInlineImage(file)) {
        val shape = RoundedCornerShape(Waddle.radius.control)
        AsyncImage(
            model = imageModelOf(file),
            contentDescription = file.name ?: stringResource(R.string.message_attachment),
            contentScale = ContentScale.Crop,
            modifier = Modifier
                .padding(top = Waddle.space.xxs)
                .widthIn(max = MEDIA_MAX_WIDTH)
                .heightIn(max = MEDIA_MAX_HEIGHT)
                .aspectRatio(aspectRatioOf(file))
                .clip(shape)
                .background(Waddle.colors.surface2)
                .border(1.dp, Waddle.colors.border, shape)
                .clickable(onClickLabel = stringResource(R.string.media_viewer_open)) { viewerOpen = true },
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
        icon = if (file.encrypted != null) Icons.Outlined.Lock else Icons.AutoMirrored.Outlined.InsertDriveFile,
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
            style = MetaStyle,
            color = Waddle.colors.dangerText,
        )
    }
}

/** A file on a card: teal-tinted glyph tile, name, mono type and size. */
@Composable
private fun FileCard(
    icon: ImageVector,
    title: String,
    subtitle: String?,
    onClick: (() -> Unit)?,
) {
    val colors = Waddle.colors
    val shape = RoundedCornerShape(Waddle.radius.control)
    val body: @Composable () -> Unit = {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
            modifier = Modifier.padding(Waddle.space.sm),
        ) {
            Box(
                contentAlignment = Alignment.Center,
                modifier = Modifier
                    .size(Waddle.control.md)
                    .background(colors.actionSubtle, RoundedCornerShape(Waddle.radius.control)),
            ) {
                Icon(icon, contentDescription = null, tint = colors.actionText, modifier = Modifier.size(FILE_ICON))
            }
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = title,
                    style = Waddle.type.control,
                    color = colors.fg,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                subtitle?.let {
                    Text(text = it, style = Waddle.type.time, color = colors.fgMuted, maxLines = 1)
                }
            }
        }
    }
    val sized = Modifier
        .padding(top = Waddle.space.xxs)
        .widthIn(max = FILE_CARD_MAX_WIDTH)
        .fillMaxWidth()
    if (onClick != null) {
        Surface(
            onClick = onClick,
            shape = shape,
            color = colors.surface,
            border = BorderStroke(1.dp, colors.border),
            modifier = sized,
        ) { body() }
    } else {
        Surface(shape = shape, color = colors.surface, border = BorderStroke(1.dp, colors.border), modifier = sized) {
            body()
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

/**
 * Full-screen image viewer on the night ground, edge to edge: the image
 * sits inside the system bars, a close button sits top-end, and a tap
 * anywhere or back also dismisses.
 */
@Composable
private fun MediaViewerDialog(model: Any, contentDescription: String?, onDismiss: () -> Unit) {
    val colors = Waddle.colors
    Dialog(
        onDismissRequest = onDismiss,
        properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
    ) {
        Box(
            modifier = Modifier
                .fillMaxSize()
                .background(colors.nightGround)
                .clickable(onClickLabel = stringResource(R.string.media_viewer_close), onClick = onDismiss),
        ) {
            AsyncImage(
                model = model,
                contentDescription = contentDescription,
                contentScale = ContentScale.Fit,
                modifier = Modifier
                    .fillMaxSize()
                    .systemBarsPadding(),
            )
            IconButton(
                onClick = onDismiss,
                colors = IconButtonDefaults.iconButtonColors(
                    containerColor = colors.nightGround,
                    contentColor = colors.nightForeground,
                ),
                modifier = Modifier
                    .align(Alignment.TopEnd)
                    .systemBarsPadding()
                    .padding(Waddle.space.xs),
            ) {
                Icon(Icons.Outlined.Close, contentDescription = stringResource(R.string.media_viewer_close))
            }
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
            .clickable(onClickLabel = stringResource(R.string.media_viewer_open)) { viewerOpen = true },
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
internal fun stickerFileOf(item: TimelineItem): WaddleSharedFile? {
    if (!item.isSticker) return null
    return item.sharedFiles.firstOrNull(::isInlineImage)
}

/** A body that is nothing but an image URL renders as the image. */
internal fun inlineImageUrlOf(item: TimelineItem, stickerFile: WaddleSharedFile?): String? =
    item.body.trim().takeIf { stickerFile == null && item.sharedFiles.isEmpty() && isImageUrl(item.body) }

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

private val MEDIA_MAX_WIDTH: Dp = 320.dp
private val MEDIA_MAX_HEIGHT: Dp = 256.dp
private val FILE_CARD_MAX_WIDTH: Dp = 320.dp
private val FILE_ICON: Dp = 20.dp
private val STICKER_SIZE: Dp = 112.dp
private const val DEFAULT_MEDIA_RATIO = 4f / 3f
private const val MIN_MEDIA_RATIO = 0.6f
private const val MAX_MEDIA_RATIO = 2.2f
private const val MAX_TYPE_LABEL = 6
private const val KIB = 1024L
private const val INLINE_IMAGE_MEDIA_TYPE = "image/gif"
