package social.waddle.android.client

import social.waddle.android.client.prefs.EncryptedFileRef
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.SharedFileRef
import social.waddle.client.ffi.WaddleEncryptedFile
import social.waddle.client.ffi.WaddleEncryptedFileHash
import social.waddle.client.ffi.WaddleFallbackRange
import social.waddle.client.ffi.WaddleMarkupSpan
import social.waddle.client.ffi.WaddleMarkupSpanType
import social.waddle.client.ffi.WaddleReference
import social.waddle.client.ffi.WaddleReferenceType
import social.waddle.client.ffi.WaddleReplyTarget
import social.waddle.client.ffi.WaddleSendOptions
import social.waddle.client.ffi.WaddleSharedFile
import social.waddle.client.ffi.WaddleStickerRef
import social.waddle.client.ffi.WaddleThreadTarget

/**
 * Structured annotations of an outbound send: an XEP-0461 reply target
 * (with the parent body for the XEP-0428 fallback quote), an XEP-0201
 * thread target, and/or XEP-0372 mentions. Persisted with queued sends
 * so an offline reply replays with its full wire shape.
 */
data class MessageSendExtras(
    /** Id of the message being replied to (MUC: room stanza id). */
    val replyToId: String? = null,
    /** Author of the replied-to message (MUC occupant full / DM bare). */
    val replyToAuthorJid: String? = null,
    /** Parent body used to build the `> quoted` fallback prefix. */
    val replyParentBody: String? = null,
    val threadId: String? = null,
    val threadParent: String? = null,
    /** Completed XEP-0363 uploads to attach (XEP-0447 metadata). */
    val sharedFiles: List<SharedFileRef> = emptyList(),
    /** XEP-0372 mentions, offsets over the pre-fallback send body. */
    val mentions: List<MentionRef> = emptyList(),
    /** XEP-0394 markup, offsets over the pre-fallback send body. */
    val markup: List<MarkupRef> = emptyList(),
    /**
     * XEP-0449 sticker: the `<sticker/>` pack reference plus the image
     * as an inline shared file with desc/hashes. Persisted with queued
     * sends — the pack item and its upload URL are durable.
     */
    val sticker: StickerSendRef? = null,
    /**
     * Fresh `urn:waddle:link-preview:0` token minted for this send.
     * NOT persisted with queued sends — tokens expire in minutes, and
     * an offline replay must not attach a stale one.
     */
    val linkPreviewToken: String? = null,
) {
    val hasReply: Boolean get() = replyToId != null && replyToAuthorJid != null
}

/** Rebuild the extras a queued send was persisted with (replay path). */
internal fun QueuedOutboundMessage.sendExtras(): MessageSendExtras? {
    val extras = MessageSendExtras(
        replyToId = replyToId,
        replyToAuthorJid = replyToAuthorJid,
        replyParentBody = replyParentBody,
        threadId = threadId,
        threadParent = threadParent,
        sharedFiles = sharedFiles,
        mentions = mentions,
        markup = markup,
        sticker = sticker,
    )
    // A plain queued body carries no annotations at all.
    return extras.takeIf { it != MessageSendExtras() }
}

/**
 * Web `buildReplyFallbackPrefix` parity: every parent line quoted with
 * `> `, terminated by a blank line separating quote from reply.
 */
fun buildReplyFallbackPrefix(parentBody: String): String =
    parentBody.lineSequence().joinToString("\n") { "> $it" } + "\n\n"

/**
 * The final wire body and options for a send: the reply fallback quote
 * is prepended to [body] and marked with its XEP-0428 range in Unicode
 * code points (NOT UTF-16 units — supplementary-plane emoji in the
 * parent would desync the range for other clients otherwise).
 */
internal fun preparedSend(
    stanzaId: String,
    body: String,
    extras: MessageSendExtras?,
): Pair<String, WaddleSendOptions> {
    val base = sendOptionsFor(stanzaId)
    if (extras == null) return body to base
    var finalBody = body
    var fallback: WaddleFallbackRange? = null
    var reply: WaddleReplyTarget? = null
    if (extras.hasReply) {
        reply = WaddleReplyTarget(
            authorJid = checkNotNull(extras.replyToAuthorJid),
            messageId = checkNotNull(extras.replyToId),
        )
        // Web parity: an empty parent (attachment-only message) gets
        // no fallback quote — "> \n\n" on the wire would render as a
        // stray quote line on non-XEP-0428 clients.
        extras.replyParentBody?.takeIf { it.isNotEmpty() }?.let { parent ->
            val prefix = buildReplyFallbackPrefix(parent)
            finalBody = prefix + body
            fallback = WaddleFallbackRange(
                start = 0u,
                end = prefix.codePointCount(0, prefix.length).toUInt(),
            )
        }
    }
    val thread = extras.threadId?.let { WaddleThreadTarget(id = it, parent = extras.threadParent) }
    val files = extras.sharedFiles.map { ref ->
        WaddleSharedFile(
            url = ref.url,
            name = ref.name,
            mediaType = ref.mediaType,
            size = ref.sizeBytes?.toULong(),
            // Web-parity: dimensions are not probed on send.
            width = null,
            height = null,
            // XEP-0449 sticker metadata; plain attachments carry none.
            desc = null,
            // XEP-0300 plaintext content hashes — XEP-0448 requires at
            // least one on encrypted sends; the Rust builder enforces it.
            hashes = ref.hashes.map { it.toFfi() },
            // The generated FFI record keeps its String field (wire
            // contract); typed → wire happens only at this boundary.
            disposition = ref.disposition.wire,
            encrypted = ref.encrypted?.toFfi(),
        )
    }
    return finalBody to base.copy(
        reply = reply,
        fallback = fallback,
        thread = thread,
        sharedFiles = files + listOfNotNull(extras.sticker?.let(::stickerSharedFile)),
        sticker = extras.sticker?.let { ref ->
            WaddleStickerRef(packId = ref.packId, packJid = null, packNode = null)
        },
        references = mentionReferences(extras.mentions, shiftBy = fallback?.end ?: 0u),
        markupSpans = markupWireSpans(extras.markup, shiftBy = fallback?.end ?: 0u),
        linkPreviewToken = extras.linkPreviewToken,
    )
}

/** FFI twin of the persisted XEP-0448 envelope. */
private fun EncryptedFileRef.toFfi(): WaddleEncryptedFile = WaddleEncryptedFile(
    cipher = cipher,
    keyB64 = keyB64,
    ivB64 = ivB64,
    hashes = hashes.map { WaddleEncryptedFileHash(algo = it.algo, valueB64 = it.valueB64) },
    sources = sources,
)

/**
 * The XEP-0447 file a sticker send attaches: inline disposition, the
 * mandatory lang-less desc, and the XEP-0300 content hashes — the wire
 * shape XEP-0449 mandates alongside the `<sticker/>` marker.
 */
private fun stickerSharedFile(ref: StickerSendRef): WaddleSharedFile = WaddleSharedFile(
    url = ref.url,
    name = null,
    mediaType = ref.mediaType,
    size = ref.sizeBytes?.toULong(),
    width = ref.width?.toUInt(),
    height = ref.height?.toUInt(),
    desc = ref.desc,
    hashes = ref.hashes.map { it.toFfi() },
    disposition = FileDisposition.INLINE.wire,
    encrypted = null,
)

/**
 * XEP-0394 wire spans for the composed markup, rebased onto the FINAL
 * wire body exactly like [mentionReferences]: a prepended reply
 * fallback quote shifts every offset right by its code-point length.
 * Empty or inverted spans are dropped.
 */
private fun markupWireSpans(markup: List<MarkupRef>, shiftBy: UInt): List<WaddleMarkupSpan> =
    markup.mapNotNull { span ->
        if (span.end <= span.begin) return@mapNotNull null
        WaddleMarkupSpan(
            spanType = span.type.wireType(),
            start = span.begin + shiftBy,
            end = span.end + shiftBy,
            uri = null,
        )
    }

private fun MarkupRefType.wireType(): WaddleMarkupSpanType = when (this) {
    MarkupRefType.BOLD -> WaddleMarkupSpanType.BOLD
    MarkupRefType.ITALIC -> WaddleMarkupSpanType.ITALIC
    MarkupRefType.STRIKETHROUGH -> WaddleMarkupSpanType.STRIKETHROUGH
    MarkupRefType.CODE -> WaddleMarkupSpanType.CODE
    MarkupRefType.CODE_BLOCK -> WaddleMarkupSpanType.CODE_BLOCK
    MarkupRefType.BLOCKQUOTE -> WaddleMarkupSpanType.BLOCKQUOTE
}

/**
 * XEP-0372 references for the accepted mentions, rebased onto the FINAL
 * wire body: when a reply fallback quote was prepended, every offset
 * shifts right by the prefix's code-point length (web
 * `shiftReferenceOffsets` parity). Empty or inverted spans are dropped —
 * `(0, 0)` is the wire's anchor-only sentinel and must not be emitted
 * for a body-positioned mention.
 */
private fun mentionReferences(mentions: List<MentionRef>, shiftBy: UInt): List<WaddleReference> =
    mentions.mapNotNull { mention ->
        if (mention.end <= mention.begin) return@mapNotNull null
        WaddleReference(
            refType = WaddleReferenceType.Mention,
            uri = mention.uri,
            begin = mention.begin + shiftBy,
            end = mention.end + shiftBy,
            anchor = null,
        )
    }

/**
 * Inbound twin of the fallback marking: drop the code-point range
 * `[start, end)` from a body so the quoted prefix never renders twice.
 * Out-of-bounds or inverted ranges return the body untouched — the
 * attributes are attacker-controlled raw u32, so validation happens in
 * the Long domain (a `toInt()` of a value ≥ 2^31 flips negative, would
 * slip past an Int-domain bound check, and crash offsetByCodePoints).
 */
fun stripReplyFallback(body: String, start: UInt?, end: UInt?): String {
    if (start == null || end == null) return body
    val startCp = start.toLong()
    val endCp = end.toLong()
    val total = body.codePointCount(0, body.length).toLong()
    if (endCp <= startCp || endCp > total) return body
    val startIndex = body.offsetByCodePoints(0, startCp.toInt())
    val endIndex = body.offsetByCodePoints(0, endCp.toInt())
    return body.removeRange(startIndex, endIndex)
}
