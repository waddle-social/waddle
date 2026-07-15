package social.waddle.android.client.prefs

import kotlinx.serialization.Serializable

/**
 * One persisted outbound send awaiting replay, stored as a JSON list in
 * [SessionPrefs] (the Android analog of web localStorage
 * `waddle.chat.outbound-queue`). Survives process death; replayed in
 * enqueue order on the next `SessionReady`.
 */
@Serializable
data class QueuedOutboundMessage(
    val conversationJid: String,
    val isGroupchat: Boolean,
    val body: String,
    /**
     * Client-generated XEP-0359 origin-id. The replay sends with this
     * SAME id, so the eventual MUC echo / XEP-0198 ack reconciles with
     * the optimistic pending row exactly like a live send would.
     */
    val clientStanzaId: String,
    val enqueuedAtMillis: Long,
)
