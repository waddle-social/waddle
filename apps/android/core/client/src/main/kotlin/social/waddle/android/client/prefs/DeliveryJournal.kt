package social.waddle.android.client.prefs

import kotlinx.serialization.Serializable
import social.waddle.client.ffi.WaddleConnectionGeneration
import social.waddle.client.ffi.WaddleDeliveryAttemptId
import java.util.UUID
import social.waddle.client.ffi.WaddleDeliveryAttemptRef as FfiDeliveryAttemptRef
import social.waddle.client.ffi.WaddleDeliveryAttemptTransition as FfiDeliveryAttemptTransition

/**
 * The only durable authority for Android delivery state.
 *
 * Every owner lives in an independent bucket. Mutations must select the
 * bucket by bare JID and preserve every foreign bucket byte-for-byte.
 */
@Serializable
data class DeliveryJournal(
    val schemaVersion: Int = CURRENT_SCHEMA_VERSION,
    val activeOwnerBareJid: String? = null,
    val owners: Map<String, DeliveryOwnerJournal> = emptyMap(),
) {
    init {
        require(schemaVersion == CURRENT_SCHEMA_VERSION) {
            "unsupported delivery journal schema version: $schemaVersion"
        }
        owners.forEach { (ownerBareJid, owner) ->
            owner.terminalReceipt?.let { receipt ->
                require(receipt.owner.value == ownerBareJid) {
                    "terminal receipt owner must match its journal bucket"
                }
            }
        }
    }

    companion object {
        const val CURRENT_SCHEMA_VERSION = 1
    }
}

@Serializable
data class DeliveryOwnerJournal(
    val session: DeliverySessionMetadata? = null,
    val activeAttempt: DeliveryAttemptRef? = null,
    val resumeTransitionReceipts: List<CommittedResumeTransition> = emptyList(),
    val nextSequence: Long = 1,
    val sm: SmResumeSlot = SmResumeSlot(),
    val outboundRows: List<QueuedOutboundMessage> = emptyList(),
    val terminalIntents: List<DeliveryTerminalIntent> = emptyList(),
    val terminalReceipt: TerminalReceipt? = null,
) {
    init {
        require(nextSequence > 0) { "delivery sequence must remain positive" }
    }
}

@Serializable
data class DeliverySessionMetadata(
    val sessionId: String,
)

@Serializable
@JvmInline
value class DeliveryAttemptId(val value: String) {
    init {
        require(runCatching { UUID.fromString(value) }.isSuccess) {
            "delivery attempt id must be a UUID"
        }
    }

    companion object {
        fun random(): DeliveryAttemptId = DeliveryAttemptId(UUID.randomUUID().toString())
    }
}

@Serializable
@JvmInline
value class NativeConnectionGeneration(val value: ULong) {
    companion object {
        fun initial(): NativeConnectionGeneration = NativeConnectionGeneration(0u)
    }

    fun next(): NativeConnectionGeneration {
        require(value < ULong.MAX_VALUE) { "native connection generation exhausted" }
        return NativeConnectionGeneration(value + 1u)
    }
}

/** Durable identity of one native connection attempt. */
@Serializable
data class DeliveryAttemptRef(
    val ownerBareJid: String,
    val attemptId: DeliveryAttemptId,
    val nativeGeneration: NativeConnectionGeneration,
)

@Serializable
data class DeliveryAttemptTransition(
    val old: DeliveryAttemptRef,
    val fresh: DeliveryAttemptRef,
)

fun DeliveryAttemptRef.toFfi(): FfiDeliveryAttemptRef = FfiDeliveryAttemptRef(
    attemptId = WaddleDeliveryAttemptId(attemptId.value),
    connectionGeneration = WaddleConnectionGeneration(nativeGeneration.value),
)

fun FfiDeliveryAttemptRef.toDomain(ownerBareJid: String): DeliveryAttemptRef =
    DeliveryAttemptRef(
        ownerBareJid = ownerBareJid,
        attemptId = DeliveryAttemptId(attemptId.value),
        nativeGeneration = NativeConnectionGeneration(connectionGeneration.value),
    )

fun FfiDeliveryAttemptTransition.toDomain(
    ownerBareJid: String,
): DeliveryAttemptTransition = DeliveryAttemptTransition(
    old = old.toDomain(ownerBareJid),
    fresh = fresh.toDomain(ownerBareJid),
)

@Serializable
data class CommittedResumeTransition(
    val transition: DeliveryAttemptTransition,
    val affectedSetDigest: String,
    val smVersion: Long,
    val committedAtMillis: Long,
    val terminalAtMillis: Long? = null,
)

@Serializable
data class SmResumeSlot(
    /** Last accepted callback version, including clear callbacks. */
    val version: Long = 0,
    /** Highest version atomically consumed or cleared. */
    val tombstoneVersion: Long = 0,
    /** Exact attempt that authored [version]. */
    val writerAttempt: DeliveryAttemptRef? = null,
    val snapshot: SmResumeSnapshot? = null,
) {
    init {
        require(version >= 0) { "SM version must be non-negative" }
        require(tombstoneVersion >= 0) { "SM tombstone version must be non-negative" }
        require(tombstoneVersion <= version) { "SM tombstone cannot exceed version" }
        require(snapshot != null || tombstoneVersion == version) {
            "an empty SM slot must be tombstoned at its latest version"
        }
        require(version == 0L || writerAttempt != null) {
            "a versioned SM slot requires an exact writer attempt"
        }
    }
}

@Serializable
@JvmInline
value class DeliveryTerminalIntentId(val value: String) {
    init {
        require(runCatching { UUID.fromString(value) }.isSuccess) {
            "terminal intent id must be a UUID"
        }
    }

    companion object {
        fun random(): DeliveryTerminalIntentId =
            DeliveryTerminalIntentId(UUID.randomUUID().toString())
    }
}

@Serializable
enum class DeliveryTerminalKind {
    ACK,
    NATIVE_FAILURE,
    NONRETRYABLE_DELETE,
}

/**
 * Durable terminal work. [expectedOwnership] and [row] are the complete CAS
 * proof captured before the row is parked in [OutboundOwnership.Terminal].
 */
@Serializable
data class DeliveryTerminalIntent(
    val id: DeliveryTerminalIntentId,
    val row: DeliveryRowIdentity,
    val expectedOwnership: OutboundOwnership.NativeOwned,
    val kind: DeliveryTerminalKind,
    val createdAtMillis: Long,
)

/** One exact callback/persistence key; stanza IDs alone are never keys. */
@Serializable
data class DeliveryCallbackRef(
    val row: DeliveryRowIdentity,
    val attempt: DeliveryAttemptRef,
)

data class DeliveryJournalMutation<T>(
    val journal: DeliveryJournal,
    val result: T,
)
