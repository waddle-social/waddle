package social.waddle.android.client

import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.SharedFileRef
import social.waddle.client.ffi.WaddleFallbackRange
import social.waddle.client.ffi.WaddleReplyTarget
import social.waddle.client.ffi.WaddleSendOptions
import social.waddle.client.ffi.WaddleSharedFile
import social.waddle.client.ffi.WaddleThreadTarget

/**
 * Structured annotations of an outbound send: an XEP-0461 reply target
 * (with the parent body for the XEP-0428 fallback quote) and/or an
 * XEP-0201 thread target. Persisted with queued sends so an offline
 * reply replays with its full wire shape.
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
) {
    val hasReply: Boolean get() = replyToId != null && replyToAuthorJid != null
}

/** Rebuild the extras a queued send was persisted with (replay path). */
internal fun QueuedOutboundMessage.sendExtras(): MessageSendExtras? {
    if (replyToId == null && threadId == null && sharedFiles.isEmpty()) return null
    return MessageSendExtras(
        replyToId = replyToId,
        replyToAuthorJid = replyToAuthorJid,
        replyParentBody = replyParentBody,
        threadId = threadId,
        threadParent = threadParent,
        sharedFiles = sharedFiles,
    )
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
            // The generated FFI record keeps its String field (wire
            // contract); typed → wire happens only at this boundary.
            disposition = ref.disposition.wire,
            encrypted = null,
        )
    }
    return finalBody to base.copy(
        reply = reply,
        fallback = fallback,
        thread = thread,
        sharedFiles = files,
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
