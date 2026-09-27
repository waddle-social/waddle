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
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import social.waddle.android.client.PeerAvatarSource
import social.waddle.android.client.normalizedBareJid
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
 * Composition watches the JID (lazy fetch + revalidation while on
 * screen); policy lives in the core `PeerAvatarRepository`.
 */
@Composable
fun PeerAvatar(
    jid: String?,
    displayName: String,
    size: Dp,
    modifier: Modifier = Modifier,
) {
    val source = LocalPeerAvatars.current
    val key = jid?.let(::normalizedBareJid)?.takeIf { it.isNotEmpty() }
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
    // Watching (not a composable-side timer) keeps it fresh: the
    // repository revalidates watched JIDs on its own clock and on every
    // new session, so no effect here ever loops or delays.
    DisposableEffect(source, key) {
        val release = source.watch(key)
        onDispose { release() }
    }
    val avatars by source.avatars.collectAsState()
    val current = avatars[key] ?: return null
    val data = current.data
    return rememberDecodedAvatar("$key#${current.id}") { cacheKey -> DecodedAvatars.decode(cacheKey, data) }
}

/**
 * The decoded image for [cacheKey] (bare JID + item id). The holder is
 * KEYED by [cacheKey]: a new id, or a slot reused for another person,
 * starts from that key's own cache entry (or nothing) — never from the
 * previous key's bitmap.
 */
@Composable
internal fun rememberDecodedAvatar(
    cacheKey: String,
    cached: (String) -> ImageBitmap? = DecodedAvatars::get,
    decode: (String) -> ImageBitmap?,
): ImageBitmap? {
    val image = remember(cacheKey) { mutableStateOf(cached(cacheKey)) }
    LaunchedEffect(cacheKey) {
        if (image.value == null) image.value = withContext(Dispatchers.Default) { decode(cacheKey) }
    }
    return image.value
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

/**
 * Process-wide decoded-bitmap cache keyed by (bare JID, item id),
 * bounded by decoded bytes rather than entry count.
 */
internal object DecodedAvatars {
    private const val BUDGET_BYTES = 4 * 1024 * 1024
    private const val MAX_EDGE_PX = 256

    private val cache = object : LruCache<String, ImageBitmap>(BUDGET_BYTES) {
        override fun sizeOf(key: String, value: ImageBitmap): Int = value.asAndroidBitmap().byteCount
    }

    fun get(key: String): ImageBitmap? = cache.get(key)

    fun decode(key: String, bytes: ByteArray): ImageBitmap? {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
        val options = BitmapFactory.Options().apply {
            inSampleSize = sampleSizeFor(maxOf(bounds.outWidth, bounds.outHeight), MAX_EDGE_PX)
        }
        val image = BitmapFactory.decodeByteArray(bytes, 0, bytes.size, options)?.asImageBitmap() ?: return null
        cache.put(key, image)
        return image
    }
}

/**
 * Power-of-two `inSampleSize` bringing the LONGER edge down to at most
 * twice [maxEdge] (BitmapFactory only honors powers of two), so a tall
 * or wide image is bounded too.
 */
internal fun sampleSizeFor(longerEdge: Int, maxEdge: Int): Int {
    var sample = 1
    while (longerEdge / (sample * 2) >= maxEdge) sample *= 2
    return sample
}

/** Leading-avatar size of the app's people list rows (Material3 ListItem). */
val LIST_AVATAR_SIZE: Dp = 40.dp

private const val INITIALS_SCALE = 0.4f
