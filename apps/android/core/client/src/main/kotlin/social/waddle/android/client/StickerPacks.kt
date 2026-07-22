package social.waddle.android.client

import kotlinx.serialization.Serializable
import social.waddle.client.ffi.WaddlePackSticker
import social.waddle.client.ffi.WaddleStickerHash
import social.waddle.client.ffi.WaddleStickerPack

/**
 * Kotlin-domain XEP-0449 sticker types mirroring the FFI records with
 * JVM-friendly (and serializable) field types: the FFI's unsigned
 * integers become Long/Int here, and [StickerHash] doubles as the
 * persisted hash shape of a queued sticker send.
 */

/** One XEP-0300 `urn:xmpp:hashes:2` hash (`sha-256` in v1). */
@Serializable
data class StickerHash(
    val algo: String,
    val valueB64: String,
)

/** One sticker inside a pack (FFI `WaddlePackSticker` mirror). */
data class StickerItem(
    /** Mandatory lang-less textual fallback (usually emoji). */
    val desc: String,
    val mediaType: String? = null,
    val sizeBytes: Long? = null,
    val width: Int? = null,
    val height: Int? = null,
    /** ≥1 per XEP-0449; all stickers in a pack share one algorithm. */
    val hashes: List<StickerHash> = emptyList(),
    /** XEP-0447 source URLs the image can be fetched from. */
    val sources: List<String> = emptyList(),
    /** XEP-0449 `<suggest/>` texts clients may replace with the sticker. */
    val suggests: List<String> = emptyList(),
)

/** One pubsub item on `urn:xmpp:stickers:0` (FFI `WaddleStickerPack` mirror). */
data class StickerPack(
    /**
     * Pack id derived from the pack hash. Publishes recompute it from
     * content FFI-side — the local value is display state, never trusted.
     */
    val id: String,
    val name: String? = null,
    val summary: String? = null,
    /** `<restricted/>`: the pack may not be imported by other users. */
    val restricted: Boolean = false,
    val stickers: List<StickerItem> = emptyList(),
)

/** Typed state of the account's own PEP sticker-pack node. */
sealed interface StickerPacksResult {
    /** The node answered with at least one pack. */
    data class Ready(val packs: List<StickerPack>) : StickerPacksResult

    /** The node answered and holds no packs (first-run state). */
    data object Empty : StickerPacksResult

    /** Offline or the fetch failed; the next picker open retries. */
    data object Unavailable : StickerPacksResult
}

/**
 * A sticker riding an outbound send: the XEP-0449 `<sticker/>` pack
 * reference plus the image as an inline XEP-0447 shared file carrying
 * the mandatory lang-less desc and the XEP-0300 content hashes.
 * Serializable — queued sticker sends replay with their full wire shape.
 */
@Serializable
data class StickerSendRef(
    /** Pack id for the `<sticker/>` marker (own PEP node). */
    val packId: String,
    /** The sticker's lang-less desc; also the message body. */
    val desc: String,
    /** XEP-0447 source URL of the sticker image. */
    val url: String,
    val mediaType: String? = null,
    val sizeBytes: Long? = null,
    val width: Int? = null,
    val height: Int? = null,
    val hashes: List<StickerHash> = emptyList(),
)

internal fun WaddleStickerPack.toDomain(): StickerPack = StickerPack(
    id = id,
    name = name,
    summary = summary,
    restricted = restricted,
    stickers = stickers.map { it.toDomain() },
)

internal fun WaddlePackSticker.toDomain(): StickerItem = StickerItem(
    desc = desc,
    mediaType = mediaType,
    sizeBytes = size?.toLong(),
    width = width?.toInt(),
    height = height?.toInt(),
    hashes = hashes.map { StickerHash(algo = it.algo, valueB64 = it.valueB64) },
    sources = sources,
    suggests = suggests,
)

internal fun StickerItem.toFfi(): WaddlePackSticker = WaddlePackSticker(
    desc = desc,
    mediaType = mediaType,
    size = sizeBytes?.toULong(),
    width = width?.toUInt(),
    height = height?.toUInt(),
    hashes = hashes.map { it.toFfi() },
    sources = sources,
    suggests = suggests,
)

internal fun StickerHash.toFfi(): WaddleStickerHash =
    WaddleStickerHash(algo = algo, valueB64 = valueB64)
