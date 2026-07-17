package social.waddle.android.client

import social.waddle.android.client.prefs.DeliveryRowIdentity
import social.waddle.client.ffi.WaddleSendMessageOutcome
import social.waddle.client.ffi.WaddleSendOptions

/**
 * Manager-level send outcome: the raw FFI [WaddleSendMessageOutcome]
 * (unchanged — it is the FFI contract) plus the exact durable delivery
 * identity when the send was journaled. Callers may project the stanza ID
 * for optimistic UI, but lifecycle mutation uses all of [delivery].
 */
data class SendResult(
    val outcome: WaddleSendMessageOutcome,
    val delivery: DeliveryOutcomeRef? = null,
) {
    val queued: Boolean
        get() = delivery != null && outcome !is WaddleSendMessageOutcome.Sent

    val deliveryIdentity: DeliveryRowIdentity?
        get() = delivery?.identity
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
)
