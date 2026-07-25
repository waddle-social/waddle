package social.waddle.android.client

import social.waddle.client.ffi.WaddleFeedEntry
import social.waddle.client.ffi.WaddleFeedSourceKind
import java.time.Instant

/**
 * Kotlin-domain XEP-0472 community feed types mirroring the FFI
 * records 1:1 with JVM-friendly field types: the FFI's unix-millis
 * timestamp becomes [Instant] here.
 */

/**
 * PEP-bridge source kind on a bridged feed entry (`urn:waddle:
 * feed-source:0`); manual posts carry none.
 */
enum class FeedSourceKind {
    MOOD,
    ACTIVITY,
    TUNE,
    AVATAR,
    VCARD,
    RSVP,
}

/** One XEP-0472 feed entry (FFI `WaddleFeedEntry` mirror). */
data class FeedEntry(
    /** Pubsub item id (`post-<uuid>` for manual posts). */
    val id: String,
    val title: String? = null,
    val body: String,
    /** Author bare JID (server-stamped on member posts) or display name. */
    val author: String? = null,
    val published: Instant? = null,
    val link: String? = null,
    /** PEP-bridge source kind; `null` on manual posts. */
    val source: FeedSourceKind? = null,
)

internal fun WaddleFeedEntry.toDomain(): FeedEntry = FeedEntry(
    id = id,
    title = title,
    body = body,
    author = author,
    published = publishedEpochMs?.let(Instant::ofEpochMilli),
    link = link,
    source = source?.toDomain(),
)

internal fun WaddleFeedSourceKind.toDomain(): FeedSourceKind = when (this) {
    WaddleFeedSourceKind.MOOD -> FeedSourceKind.MOOD
    WaddleFeedSourceKind.ACTIVITY -> FeedSourceKind.ACTIVITY
    WaddleFeedSourceKind.TUNE -> FeedSourceKind.TUNE
    WaddleFeedSourceKind.AVATAR -> FeedSourceKind.AVATAR
    WaddleFeedSourceKind.VCARD -> FeedSourceKind.VCARD
    WaddleFeedSourceKind.RSVP -> FeedSourceKind.RSVP
}
