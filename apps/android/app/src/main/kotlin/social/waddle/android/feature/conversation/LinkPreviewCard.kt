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
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.PlayCircle
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
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
    val href = httpsHrefOrNull(preview.originalUrl)
    val open: (() -> Unit)? = href?.let { url -> { runCatching { uriHandler.openUri(url) } } }
    Surface(
        shape = RoundedCornerShape(8.dp),
        color = MaterialTheme.colorScheme.surfaceContainerHighest,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
        onClick = open ?: {},
        enabled = open != null,
        modifier = modifier.fillMaxWidth().padding(top = 2.dp),
    ) {
        Column(
            modifier = Modifier.padding(8.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            linkPreviewHost(preview.originalUrl)?.let { host ->
                Text(
                    text = host,
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            PreviewMedia(preview, trustedMediaOrigin)
            preview.title?.let { title ->
                Text(
                    text = title,
                    style = MaterialTheme.typography.bodyMedium,
                    fontWeight = FontWeight.Medium,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            preview.description?.let { description ->
                Text(
                    text = description,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
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
                    .fillMaxWidth()
                    .heightIn(max = 192.dp)
                    .clip(RoundedCornerShape(6.dp)),
            )
            if (playable) PlayGlyph()
        }
        preview.remoteMediaUnavailable -> Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(
                Icons.Outlined.ErrorOutline,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.size(14.dp),
            )
            Text(
                text = stringResource(R.string.link_preview_remote_media_unavailable),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(start = 4.dp),
            )
        }
        else -> Unit
    }
}

/** Explicit play affordance over video/player posters (web parity). */
@Composable
private fun PlayGlyph() {
    Surface(
        shape = CircleShape,
        color = MaterialTheme.colorScheme.surface.copy(alpha = 0.7f),
    ) {
        Icon(
            Icons.Outlined.PlayCircle,
            contentDescription = stringResource(R.string.link_preview_play),
            modifier = Modifier.padding(4.dp).size(32.dp),
        )
    }
}
