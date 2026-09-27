package social.waddle.android.avatar

import android.graphics.BitmapFactory
import android.util.LruCache
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import social.waddle.android.client.PeerAvatarSource
import social.waddle.android.jid.bareJidOf
import social.waddle.android.theme.consistentColor

/**
 * The session's peer-avatar cache. `null` (screen tests, previews)
 * renders initials only and never touches the network.
 */
val LocalPeerAvatars = staticCompositionLocalOf<PeerAvatarSource?> { null }

/**
 * A person's avatar, everywhere the app shows another user: their
 * XEP-0084 image when one is published, else initials on their XEP-0392
 * consistent color (web `AppAvatar` parity). [jid] is the REAL bare JID
 * — never a nick or a guess; `null` when unknown renders initials.
 * The first composition triggers the lazy fetch; policy lives in the
 * core `PeerAvatarRepository`.
 */
@Composable
fun PeerAvatar(
    jid: String?,
    displayName: String,
    size: Dp,
    modifier: Modifier = Modifier,
) {
    val source = LocalPeerAvatars.current
    val key = jid?.let(::bareJidOf)?.trim()?.takeIf { it.isNotEmpty() }
    val image = if (source != null && key != null) rememberAvatarImage(source, key) else null
    Box(
        modifier = modifier
            .size(size)
            .clip(CircleShape)
            .background(if (image == null) consistentColor(displayName) else Color.Transparent),
        contentAlignment = Alignment.Center,
    ) {
        if (image != null) {
            Image(
                bitmap = image,
                contentDescription = null,
                contentScale = ContentScale.Crop,
                modifier = Modifier.fillMaxSize(),
            )
        } else {
            Text(
                text = initialsOf(displayName),
                color = Color.White,
                fontWeight = FontWeight.Medium,
                // Scales with the circle, not the user's font scale.
                fontSize = with(LocalDensity.current) { (size * INITIALS_SCALE).toSp() },
                maxLines = 1,
            )
        }
    }
}

@Composable
private fun rememberAvatarImage(source: PeerAvatarSource, key: String): ImageBitmap? {
    val epoch by source.epoch.collectAsState()
    LaunchedEffect(source, key, epoch) { source.ensure(key) }
    val avatars by source.avatars.collectAsState()
    val current = avatars[key] ?: return null
    val cacheKey = "$key#${current.id}"
    val image by produceState(initialValue = DecodedAvatars.get(cacheKey), cacheKey) {
        if (value == null) value = withContext(Dispatchers.Default) { DecodedAvatars.decode(cacheKey, current.data) }
    }
    return image
}

/**
 * Web `AppAvatar` initials: the first character of each space-separated
 * word, uppercased, at most two.
 */
fun initialsOf(name: String): String {
    val firsts = name.split(' ')
        .filter { it.isNotEmpty() }
        .joinToString(separator = "") { word -> String(Character.toChars(word.codePointAt(0))) }
        .uppercase()
    return firsts.substring(0, firsts.offsetByCodePoints(0, minOf(2, firsts.codePointCount(0, firsts.length))))
}

/** Process-wide decoded-bitmap cache keyed by (bare JID, item id). */
private object DecodedAvatars {
    private const val MAX_ENTRIES = 128
    private const val MAX_EDGE_PX = 256

    private val cache = LruCache<String, ImageBitmap>(MAX_ENTRIES)

    fun get(key: String): ImageBitmap? = cache.get(key)

    fun decode(key: String, bytes: ByteArray): ImageBitmap? {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
        var sample = 1
        while (bounds.outWidth / (sample * 2) >= MAX_EDGE_PX && bounds.outHeight / (sample * 2) >= MAX_EDGE_PX) {
            sample *= 2
        }
        val options = BitmapFactory.Options().apply { inSampleSize = sample }
        val image = BitmapFactory.decodeByteArray(bytes, 0, bytes.size, options)?.asImageBitmap() ?: return null
        cache.put(key, image)
        return image
    }
}

/** Leading-avatar size of the app's people list rows (Material3 ListItem). */
val LIST_AVATAR_SIZE: Dp = 40.dp

private const val INITIALS_SCALE = 0.4f
