package social.waddle.android.client

import social.waddle.client.ffi.WaddleSendMessageOutcome
import social.waddle.client.ffi.WaddleSendOptions

/**
 * Manager-level send outcome: the raw FFI [WaddleSendMessageOutcome]
 * (unchanged — it is the FFI contract) plus the outbound-queue handle
 * when the send could not reach a live session. [queuedId] is the
 * client stanza id the message was persisted under; the queue replays
 * with the SAME id (XEP-0359 origin-id), so callers treat it exactly
 * like the id of a `Sent` outcome: the eventual echo collapses the
 * optimistic row and `DeliveryAcked`/`DeliveryFailed` target it.
 */
data class SendResult(
    val outcome: WaddleSendMessageOutcome,
    val queuedId: String? = null,
) {
    val queued: Boolean get() = queuedId != null
}

/**
 * Default send options carrying only the caller-chosen stanza id (the
 * Rust builder uses it as the message id AND the XEP-0359 origin-id,
 * and echoes it back as `Sent.stanzaId`).
 */
internal fun sendOptionsFor(stanzaId: String): WaddleSendOptions = WaddleSendOptions(
    stanzaId = stanzaId,
    subject = null,
    reply = null,
    fallback = null,
    thread = null,
    markupSpans = emptyList(),
    references = emptyList(),
    sharedFiles = emptyList(),
    linkPreviewToken = null,
    requestDisplayedMarker = false,
    mucPm = false,
)
