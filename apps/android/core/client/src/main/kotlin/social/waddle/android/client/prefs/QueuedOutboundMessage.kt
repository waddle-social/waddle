package social.waddle.android.client.prefs

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import social.waddle.android.client.FileDisposition
import social.waddle.android.client.MentionRef
import java.security.MessageDigest
import java.text.Normalizer
import java.util.UUID

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

@Serializable
enum class NativeOutboundPhase {
    FRESH,
    RESUME,
    FRESH_FALLBACK,
}

/** Durable authority for an outbound row. Invalid owner/phase combinations
 * are unrepresentable: browser/Kotlin replay may send [Ready] rows, while an
 * exact native connection generation exclusively owns [NativeOwned] rows.
 * [Terminal] rows are parked while their durable intent is applied and can
 * never be sent or evicted. */
@Serializable
sealed interface OutboundOwnership {
    @Serializable
    @SerialName("ready")
    data object Ready : OutboundOwnership

    @Serializable
    @SerialName("native-owned")
    data class NativeOwned(
        val attempt: DeliveryAttemptRef,
        val phase: NativeOutboundPhase,
    ) : OutboundOwnership

    @Serializable
    @SerialName("terminal")
    data class Terminal(
        val intentId: DeliveryTerminalIntentId,
    ) : OutboundOwnership
}

@Serializable
@JvmInline
value class DeliveryIncarnation(val value: String) {
    init {
        require(runCatching { UUID.fromString(value) }.isSuccess) {
            "delivery incarnation must be a UUID"
        }
    }

    companion object {
        fun random(): DeliveryIncarnation = DeliveryIncarnation(UUID.randomUUID().toString())
    }
}

@Serializable
@JvmInline
value class DeliveryPayloadDigest(val value: String) {
    init {
        require(value.matches(Regex("v1:sha256:[0-9a-f]{64}"))) {
            "unsupported delivery payload digest"
        }
    }
}

@Serializable
data class DeliveryRowIdentity(
    val ownerBareJid: String,
    val clientStanzaId: String,
    val incarnation: DeliveryIncarnation,
    val payloadDigest: DeliveryPayloadDigest,
)

/**
 * Non-wire source context. It is excluded from [DeliveryPayloadDigest] but
 * remains durable so terminal application can route UI effects after process
 * death without an ID-only side map.
 */
@Serializable
sealed interface DeliverySource {
    @Serializable
    @SerialName("composer")
    data object Composer : DeliverySource

    @Serializable
    @SerialName("direct-reply")
    data class DirectReply(
        val conversationJid: String,
        val isGroupchat: Boolean,
    ) : DeliverySource
}

data class QueuedOutboundPayload(
    val conversationJid: String,
    val isGroupchat: Boolean,
    val body: String,
    val replyToId: String? = null,
    val replyToAuthorJid: String? = null,
    val replyParentBody: String? = null,
    val threadId: String? = null,
    val threadParent: String? = null,
    val sharedFiles: List<SharedFileRef> = emptyList(),
    val mentions: List<MentionRef> = emptyList(),
)

/**
 * One persisted outbound send in [SessionPrefs]. Ready rows await Kotlin
 * replay; NativeOwned rows await native acknowledgement/failure or
 * reconnect reconciliation. Both survive process death.
 */
@Serializable
data class QueuedOutboundMessage(
    /**
     * Bare JID of the account that authored the send. The queue file is
     * shared process state: drains MUST skip (and logins prune) entries
     * from another account, or a message enqueued around logout would be
     * replayed — and misdelivered — under the next signed-in account.
     */
    val ownerBareJid: String,
    val conversationJid: String,
    val isGroupchat: Boolean,
    val body: String,
    /**
     * Client-generated XEP-0359 origin-id. The replay sends with this
     * SAME id, so the eventual MUC echo / XEP-0198 ack reconciles with
     * the optimistic pending row exactly like a live send would.
     */
    val clientStanzaId: String,
    val incarnation: DeliveryIncarnation,
    val payloadDigest: DeliveryPayloadDigest,
    val sequence: Long,
    val enqueuedAtMillis: Long,
    val ownership: OutboundOwnership = OutboundOwnership.Ready,
    val source: DeliverySource = DeliverySource.Composer,
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
) {
    val identity: DeliveryRowIdentity
        get() = DeliveryRowIdentity(
            ownerBareJid = ownerBareJid,
            clientStanzaId = clientStanzaId,
            incarnation = incarnation,
            payloadDigest = payloadDigest,
        )

    val payload: QueuedOutboundPayload
        get() = QueuedOutboundPayload(
            conversationJid = conversationJid,
            isGroupchat = isGroupchat,
            body = body,
            replyToId = replyToId,
            replyToAuthorJid = replyToAuthorJid,
            replyParentBody = replyParentBody,
            threadId = threadId,
            threadParent = threadParent,
            sharedFiles = sharedFiles,
            mentions = mentions,
        )

    init {
        require(ownerBareJid.isNotBlank()) { "delivery owner must not be blank" }
        require(clientStanzaId.isNotBlank()) { "client stanza id must not be blank" }
        require(sequence > 0) { "persisted delivery sequence must be positive" }
        require(payloadDigest == structuralDigest()) {
            "delivery payload digest does not match the stored payload"
        }
    }

    /**
     * Stable digest of validated client semantics. Delivery identity,
     * timestamps, ownership, and retry metadata are deliberately excluded.
     */
    fun structuralDigest(): DeliveryPayloadDigest = computeStructuralDigest(payload)

    companion object {
        internal fun computeStructuralDigest(
            payload: QueuedOutboundPayload,
        ): DeliveryPayloadDigest {
            val digest = MessageDigest.getInstance("SHA-256")
            fun bytes(value: String) {
                val encoded = Normalizer.normalize(value, Normalizer.Form.NFC)
                    .toByteArray(Charsets.UTF_8)
                digest.update(encoded.size.toString().toByteArray(Charsets.US_ASCII))
                digest.update(':'.code.toByte())
                digest.update(encoded)
            }
            fun field(tag: String, value: String?) {
                bytes(tag)
                val encoded = value
                    ?.let { Normalizer.normalize(it, Normalizer.Form.NFC) }
                    ?.toByteArray(Charsets.UTF_8)
                if (encoded == null) {
                    digest.update('N'.code.toByte())
                    return
                }
                digest.update('V'.code.toByte())
                digest.update(encoded.size.toString().toByteArray(Charsets.US_ASCII))
                digest.update(':'.code.toByte())
                digest.update(encoded)
            }
            field("domain", "waddle.android.delivery")
            field("version", "1")
            field("target", payload.conversationJid)
            field("stanza-kind", if (payload.isGroupchat) "groupchat" else "chat")
            field("body", payload.body)
            field("reply-id", payload.replyToId)
            field("reply-author", payload.replyToAuthorJid)
            field("reply-fallback-body", payload.replyParentBody)
            field("thread-id", payload.threadId)
            field("thread-parent", payload.threadParent)
            field("shared-file-count", payload.sharedFiles.size.toString())
            payload.sharedFiles.forEachIndexed { index, file ->
                field("shared-file[$index].url", file.url)
                field("shared-file[$index].name", file.name)
                field("shared-file[$index].media-type", file.mediaType)
                field("shared-file[$index].size", file.sizeBytes?.toString())
                field("shared-file[$index].disposition", file.disposition.name)
            }
            field("mention-count", payload.mentions.size.toString())
            payload.mentions.forEachIndexed { index, mention ->
                field("mention[$index].uri", mention.uri)
                field("mention[$index].begin", mention.begin.toString())
                field("mention[$index].end", mention.end.toString())
            }
            return DeliveryPayloadDigest(
                "v1:sha256:" + digest.digest().joinToString(separator = "") { byte ->
                    (byte.toInt() and 0xff).toString(16).padStart(2, '0')
                },
            )
        }

        fun create(
            draft: QueuedOutboundDraft,
            sequence: Long,
            ownership: OutboundOwnership = OutboundOwnership.Ready,
        ): QueuedOutboundMessage = QueuedOutboundMessage(
            ownerBareJid = draft.ownerBareJid,
            conversationJid = draft.conversationJid,
            isGroupchat = draft.isGroupchat,
            body = draft.body,
            clientStanzaId = draft.clientStanzaId,
            incarnation = draft.incarnation,
            payloadDigest = draft.payloadDigest,
            sequence = sequence,
            enqueuedAtMillis = draft.enqueuedAtMillis,
            ownership = ownership,
            source = draft.source,
            replyToId = draft.replyToId,
            replyToAuthorJid = draft.replyToAuthorJid,
            replyParentBody = draft.replyParentBody,
            threadId = draft.threadId,
            threadParent = draft.threadParent,
            sharedFiles = draft.sharedFiles,
            mentions = draft.mentions,
        )
    }
}

/**
 * Pre-persistence row. UUID and digest are computed before the DataStore edit;
 * the journal allocates only the owner's monotonic [QueuedOutboundMessage.sequence].
 */
data class QueuedOutboundDraft(
    val ownerBareJid: String,
    val conversationJid: String,
    val isGroupchat: Boolean,
    val body: String,
    val clientStanzaId: String,
    val incarnation: DeliveryIncarnation,
    val payloadDigest: DeliveryPayloadDigest,
    val enqueuedAtMillis: Long,
    val source: DeliverySource,
    val replyToId: String?,
    val replyToAuthorJid: String?,
    val replyParentBody: String?,
    val threadId: String?,
    val threadParent: String?,
    val sharedFiles: List<SharedFileRef>,
    val mentions: List<MentionRef>,
) {
    val proposedIdentity: DeliveryRowIdentity
        get() = DeliveryRowIdentity(
            ownerBareJid = ownerBareJid,
            clientStanzaId = clientStanzaId,
            incarnation = incarnation,
            payloadDigest = payloadDigest,
        )

    fun persisted(sequence: Long, ownership: OutboundOwnership): QueuedOutboundMessage =
        QueuedOutboundMessage.create(
            draft = this,
            sequence = sequence,
            ownership = ownership,
        ).also {
            require(it.payloadDigest == payloadDigest) {
                "delivery draft digest changed before persistence"
            }
        }

    companion object {
        fun create(
            ownerBareJid: String,
            clientStanzaId: String,
            enqueuedAtMillis: Long,
            payload: QueuedOutboundPayload,
            source: DeliverySource = DeliverySource.Composer,
            incarnation: DeliveryIncarnation = DeliveryIncarnation.random(),
        ): QueuedOutboundDraft {
            val digest = QueuedOutboundMessage.computeStructuralDigest(payload)
            return QueuedOutboundDraft(
                ownerBareJid = ownerBareJid,
                conversationJid = payload.conversationJid,
                isGroupchat = payload.isGroupchat,
                body = payload.body,
                clientStanzaId = clientStanzaId,
                incarnation = incarnation,
                payloadDigest = digest,
                enqueuedAtMillis = enqueuedAtMillis,
                source = source,
                replyToId = payload.replyToId,
                replyToAuthorJid = payload.replyToAuthorJid,
                replyParentBody = payload.replyParentBody,
                threadId = payload.threadId,
                threadParent = payload.threadParent,
                sharedFiles = payload.sharedFiles,
                mentions = payload.mentions,
            )
        }
    }
}
