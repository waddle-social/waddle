package social.waddle.android.client

import social.waddle.client.ffi.WaddleSendMessageOutcome
import social.waddle.client.ffi.WaddleSendOptions

/**
 * Manager-level send outcome: the raw FFI [WaddleSendMessageOutcome]
 * plus the durable outbound-intent handle. [queuedId] is present once
 * persistence succeeds, before any transport attempt; it is absent when
 * persistence/capacity fails or a synchronous permanent rejection removes
 * the row. Replay uses the SAME XEP-0359 origin-id.
 */
data class SendResult(
    val outcome: WaddleSendMessageOutcome,
    val queuedId: String? = null,
) {
    val queued: Boolean get() = queuedId != null
}

/**
 * Fresh client stanza id: the message id AND XEP-0359 origin-id of an
 * outbound send, generated manager-side so a queued replay can resend
 * under the same origin-id.
 */
internal fun newClientStanzaId(): String = java.util.UUID.randomUUID().toString()

/**
 * Default send options carrying only the caller-chosen stanza id (the
 * Rust builder uses it as the message id AND the XEP-0359 origin-id,
 * and echoes it back as `Sent.stanzaId`). Every send requests an
 * XEP-0333 displayed marker (web parity: `requestDisplayedMarker ??
 * true`) — recipients gate their DM read receipts on `<markable/>`,
 * so a send without it can never be marked read by anyone.
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
    requestDisplayedMarker = true,
    mucPm = false,
    sticker = null,
)
