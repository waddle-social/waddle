package social.waddle.android.client

import social.waddle.client.ffi.WaddleMessage
import social.waddle.client.ffi.WaddleSendOptions

/**
 * Local echo for an own 1:1 send: unlike MUC (whose room reflects
 * every message back), a DM send produces NO inbound copy for the
 * sending client, so without this row the message never enters the
 * timeline store — a peer's reaction/retraction/marker targeting the
 * origin-id would park forever, and the sender could never edit or
 * retract their own fresh message (web optimistic-row parity).
 *
 * Keyed by the client stanza id (message id AND XEP-0359 origin-id,
 * exactly what the Rust builder stamps on the wire), so the later MAM
 * copy of the same message dedupes onto this row.
 */
internal fun ownDmEcho(
    ownJid: String,
    peerJid: String,
    stanzaId: String,
    body: String,
    options: WaddleSendOptions,
): WaddleMessage = WaddleMessage(
    id = stanzaId,
    from = ownJid,
    to = bareJid(peerJid),
    body = body,
    subject = options.subject,
    messageType = "chat",
    timestamp = null,
    stanzaId = null,
    stanzaIdBy = null,
    stanzaIds = emptyList(),
    originId = stanzaId,
    replacesId = null,
    retractsId = null,
    retractionId = null,
    isRetracted = false,
    moderationTargetId = null,
    moderatedBy = null,
    moderationReason = null,
    reactionTargetId = null,
    reactionEmojis = emptyList(),
    chatState = null,
    displayedMarkerRequested = options.requestDisplayedMarker,
    displayedMarkerId = null,
    isMuc = false,
    thread = options.thread?.id,
    parentThreadId = options.thread?.parent,
    markupSpans = options.markupSpans,
    broadcastMention = null,
    mentionUris = emptyList(),
    references = options.references,
    forumPostKind = null,
    forumTitle = null,
    isSticker = false,
    linkPreviews = emptyList(),
    pinEvent = null,
    callThreadEnded = null,
    carbon = null,
    replyToId = options.reply?.messageId,
    replyToSender = options.reply?.authorJid,
    replyFallbackStart = options.fallback?.start,
    replyFallbackEnd = options.fallback?.end,
    callThread = null,
    sharedFiles = options.sharedFiles,
    mdsDisplayed = null,
)
