package social.waddle.android.client.prefs

import kotlinx.serialization.Serializable
import social.waddle.android.client.FileDisposition
import social.waddle.android.client.MarkupRef
import social.waddle.android.client.MentionRef
import social.waddle.android.client.StickerHash
import social.waddle.android.client.StickerSendRef

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
    /** PLAINTEXT size — for encrypted uploads the ciphertext is +16. */
    val sizeBytes: Long? = null,
    /** `inline` (image/video/audio/pdf) or `attachment` (web parity). */
    val disposition: FileDisposition = FileDisposition.ATTACHMENT,
    /**
     * XEP-0300 plaintext content hashes for the `<file/>` metadata.
     * XEP-0448 requires at least one on encrypted sends.
     */
    val hashes: List<StickerHash> = emptyList(),
    /** XEP-0448 envelope when the uploaded bytes are ciphertext. */
    val encrypted: EncryptedFileRef? = null,
)

/**
 * The persisted XEP-0448 `<encrypted/>` envelope of an uploaded
 * attachment: the symmetric key material recipients need to decrypt
 * the ciphertext at [sources]. Durable alongside the upload URL so
 * queued sends replay with their full wire shape.
 */
@Serializable
data class EncryptedFileRef(
    /** Cipher URN, e.g. `urn:xmpp:ciphers:aes-256-gcm-nopadding:0`. */
    val cipher: String,
    val keyB64: String,
    val ivB64: String,
    /** XEP-0300 CIPHERTEXT hashes nested inside `<encrypted/>`. */
    val hashes: List<StickerHash> = emptyList(),
    /** Ciphertext source URLs (the XEP-0363 GET URL). */
    val sources: List<String> = emptyList(),
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
    /** XEP-0394 markup spans (see `MessageSendExtras.markup`). */
    val markup: List<MarkupRef> = emptyList(),
    /** XEP-0449 sticker ref (pack item + upload URL are durable). */
    val sticker: StickerSendRef? = null,
)
