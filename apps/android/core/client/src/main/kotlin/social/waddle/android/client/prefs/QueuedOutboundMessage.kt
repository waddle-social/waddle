package social.waddle.android.client.prefs

import kotlinx.serialization.Serializable
import social.waddle.android.client.FileDisposition
import social.waddle.android.client.MentionRef

/**
 * A completed XEP-0363 upload attached to an outbound send (XEP-0447
 * metadata subset the Android client produces; the URL is durable, so
 * queued sends survive with their attachments intact).
 */
@Serializable
data class SharedFileRef(
    val url: String,
    val name: String? = null,
    val mediaType: String? = null,
    val sizeBytes: Long? = null,
    /** `inline` (image/video/audio/pdf) or `attachment` (web parity). */
    val disposition: FileDisposition = FileDisposition.ATTACHMENT,
)

/**
 * One persisted outbound send awaiting replay, stored as a JSON list in
 * [SessionPrefs] (the Android analog of web localStorage
 * `waddle.chat.outbound-queue`). Survives process death; replayed in
 * enqueue order on the next `SessionReady`.
 */
@Serializable
data class QueuedOutboundMessage(
    /**
     * Bare JID of the account that authored the send. The queue file is
     * shared process state: drains MUST skip (and logins prune) entries
     * from another account, or a message enqueued around logout would be
     * replayed — and misdelivered — under the next signed-in account.
     */
    val ownerBareJid: String = "",
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
    /** XEP-0461 reply annotation (see `MessageSendExtras`). */
    val replyToId: String? = null,
    val replyToAuthorJid: String? = null,
    val replyParentBody: String? = null,
    /** XEP-0201 thread annotation. */
    val threadId: String? = null,
    val threadParent: String? = null,
    /** XEP-0363/0447 attachments (already uploaded; URLs are durable). */
    val sharedFiles: List<SharedFileRef> = emptyList(),
    /** XEP-0372 mentions (see `MessageSendExtras.mentions`). */
    val mentions: List<MentionRef> = emptyList(),
)
