package social.waddle.android.feature.conversation

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.OpenInNew
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.PlayCircle
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import coil3.compose.AsyncImage
import social.waddle.android.R
import social.waddle.android.client.httpsHrefOrNull
import social.waddle.android.client.isTrustedCachedPreviewImageUrl
import social.waddle.android.client.linkPreviewHost
import social.waddle.android.theme.Waddle
import social.waddle.client.ffi.WaddleLinkPreview

/**
 * One `urn:waddle:link-preview:0` card (web `MessageBody.vue` parity):
 * host line, server-cached image (ONLY when its URL passes the
 * trusted-origin + XEP-0363 `link-preview-…` path check — a preview
 * image must never leak the reader's IP to a third-party host), title,
 * two-line description, and a "remote media unavailable" chip when the
 * server declined to cache. Video/player previews render their poster
 * with an explicit play affordance; every tap opens the ORIGINAL URL
 * externally, HTTPS only (v1: no in-app embeds).
 */
@Composable
fun LinkPreviewCard(
    preview: WaddleLinkPreview,
    trustedMediaOrigin: String?,
    modifier: Modifier = Modifier,
) {
    val uriHandler = LocalUriHandler.current
    val colors = Waddle.colors
    val href = httpsHrefOrNull(preview.originalUrl)
    val open: (() -> Unit)? = href?.let { url -> { runCatching { uriHandler.openUri(url) } } }
    // One teal thing per message: the link in the body. The card itself
    // is neutral, so link, rule and title never stack up in teal.
    Surface(
        shape = RoundedCornerShape(Waddle.radius.control),
        color = colors.surface,
        contentColor = colors.fg,
        border = BorderStroke(1.dp, colors.border),
        onClick = open ?: {},
        enabled = open != null,
        modifier = modifier
            .padding(top = Waddle.space.xxs)
            .widthIn(max = PREVIEW_MAX_WIDTH)
            .fillMaxWidth()
            // Without a link the card is inert, but a tap on it is still
            // not a tap on the post (which would open its thread).
            .then(if (open == null) Modifier.swallowTaps() else Modifier),
    ) {
        PreviewBody(preview, trustedMediaOrigin)
    }
}

@Composable
private fun PreviewBody(preview: WaddleLinkPreview, trustedMediaOrigin: String?) {
    val colors = Waddle.colors
    Column(
        modifier = Modifier.padding(Waddle.space.sm),
        verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
    ) {
        linkPreviewHost(preview.originalUrl)?.let { host ->
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
            ) {
                Icon(
                    Icons.AutoMirrored.Outlined.OpenInNew,
                    contentDescription = null,
                    tint = colors.fgMuted,
                    modifier = Modifier.size(SITE_ICON),
                )
                Text(
                    text = host,
                    style = MetaStyle,
                    color = colors.fgMuted,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        PreviewMedia(preview, trustedMediaOrigin)
        preview.title?.let { title ->
            Text(
                text = title,
                style = Waddle.type.control.copy(fontWeight = FontWeight.SemiBold),
                color = colors.fg,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
        preview.description?.let { description ->
            Text(
                text = description,
                style = MetaStyle,
                color = colors.fgMuted,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

@Composable
private fun PreviewMedia(preview: WaddleLinkPreview, trustedMediaOrigin: String?) {
    val image = preview.image?.takeIf {
        isTrustedCachedPreviewImageUrl(it.url, trustedMediaOrigin)
    }
    val playable = preview.video != null || preview.playerEmbed != null
    when {
        image != null -> Box(contentAlignment = Alignment.Center) {
            AsyncImage(
                model = image.url,
                contentDescription = image.alt
                    ?: preview.title
                    ?: stringResource(R.string.link_preview_image),
                contentScale = ContentScale.Crop,
                modifier = Modifier
                    .padding(vertical = Waddle.space.xxs)
                    .fillMaxWidth()
                    .heightIn(max = PREVIEW_IMAGE_MAX_HEIGHT)
                    .clip(RoundedCornerShape(Waddle.radius.tag)),
            )
            if (playable) PlayGlyph()
        }
        preview.remoteMediaUnavailable -> Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
        ) {
            Icon(
                Icons.Outlined.ErrorOutline,
                contentDescription = null,
                tint = Waddle.colors.fgMuted,
                modifier = Modifier.size(SITE_ICON),
            )
            Text(
                text = stringResource(R.string.link_preview_remote_media_unavailable),
                style = MetaStyle,
                color = Waddle.colors.fgMuted,
            )
        }
        else -> Unit
    }
}

/** Explicit play affordance over video/player posters (web parity), on the night ground. */
@Composable
private fun PlayGlyph() {
    val colors = Waddle.colors
    Surface(
        shape = CircleShape,
        color = colors.nightGround.copy(alpha = PLAY_SCRIM_ALPHA),
        contentColor = colors.nightForeground,
    ) {
        Icon(
            Icons.Outlined.PlayCircle,
            contentDescription = stringResource(R.string.link_preview_play),
            modifier = Modifier
                .padding(Waddle.space.xxs)
                .size(Waddle.control.sm),
        )
    }
}

private val PREVIEW_MAX_WIDTH = 320.dp
private val PREVIEW_IMAGE_MAX_HEIGHT = 192.dp
private val SITE_ICON = 12.dp
private const val PLAY_SCRIM_ALPHA = 0.7f
