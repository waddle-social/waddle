package social.waddle.android.client

import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryJournal
import social.waddle.android.client.prefs.DeliveryJournalMutation
import social.waddle.android.client.prefs.DeliveryOwnerBareJid
import social.waddle.android.client.prefs.DeliveryOwnerJournal
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.ProcessEpoch
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.TerminalReceipt
import social.waddle.android.client.prefs.TerminalReceiptClaimState
import social.waddle.android.client.prefs.TerminalReceiptId
import social.waddle.android.client.prefs.TerminalReceiptState

/**
 * Complete in-memory identity of one durable terminal receipt.
 *
 * Effects are dispatched before the acknowledgement write. A process death
 * after a dispatched prefix can replay the complete ordered receipt under a
 * new process epoch; consumers must use the exact delivery identity
 * idempotently.
 */
internal data class TerminalReceiptRef(
    val owner: DeliveryOwnerBareJid,
    val attempt: DeliveryAttemptRef,
    val id: TerminalReceiptId,
) {
    init {
        require(owner.value == attempt.ownerBareJid) { "terminal receipt ref owner must match attempt" }
    }
}

internal data class TerminalReceiptLease(
    val ref: TerminalReceiptRef,
    val claim: TerminalReceiptClaimState.Claimed,
)

internal data class TerminalReceiptClaimRequest(
    val ref: TerminalReceiptRef,
    val claim: TerminalReceiptClaimState.Claimed,
)

internal enum class TerminalReceiptCorruption {
    ACTIVE_OWNER_MISMATCH,
    RECEIPT_BINDING_MISMATCH,
    ACTIVE_ATTEMPT_REMAINS,
    TERMINAL_INTENTS_REMAIN,
    TERMINAL_ROW_REMAINS,
    NATIVE_OWNED_ROW_REMAINS,
    DUPLICATE_ROW,
    ROW_OWNER_MISMATCH,
    EMPTY_EFFECTS,
    DUPLICATE_EFFECT,
    EFFECT_BINDING_MISMATCH,
    REVERSED_EFFECT_ORDER,
    PERSISTED_DECODE_FAILURE,
}

/**
 * Typed identity for cleanup that could not durably release an exact receipt
 * lease. The exception which carries this evidence retains the original
 * storage/runtime cause through normal Throwable causality; protocol values
 * never carry a Throwable.
 */
internal data class TerminalReceiptCleanupEvidence(
    val lease: TerminalReceiptLease,
    val attempts: Int,
    val reason: TerminalReceiptCleanupReason,
) {
    init {
        require(attempts > 0) { "terminal receipt cleanup attempts must be positive" }
    }
}

internal sealed interface TerminalReceiptCleanupReason {
    data class Persistence(val category: TerminalReceiptCleanupFailureCategory) : TerminalReceiptCleanupReason
    data class LeaseMismatch(val current: TerminalReceiptClaimState.Claimed?) : TerminalReceiptCleanupReason
    data object ReceiptMissing : TerminalReceiptCleanupReason
    data class ReceiptReplaced(val actual: TerminalReceiptRef) : TerminalReceiptCleanupReason
    data class Corrupt(val corruption: TerminalReceiptCorruption) : TerminalReceiptCleanupReason
}

internal enum class TerminalReceiptCleanupFailureCategory {
    IO_FAILURE,
    CANCELLATION,
    CODEC_FAILURE,
    INVARIANT_FAILURE,
    RUNTIME_FAILURE,
    ERROR_FAILURE,
}

internal sealed interface TerminalReceiptCleanupResult {
    data object Released : TerminalReceiptCleanupResult
    data class Unresolved(
        val evidence: TerminalReceiptCleanupEvidence,
    ) : TerminalReceiptCleanupResult
}

internal sealed interface TerminalReceiptRecoveryCleanupResult {
    data object NoPendingLease : TerminalReceiptRecoveryCleanupResult
    data object Released : TerminalReceiptRecoveryCleanupResult
    data class Unresolved(
        val evidence: TerminalReceiptCleanupEvidence,
    ) : TerminalReceiptRecoveryCleanupResult
}

internal sealed interface TerminalReceiptFailureExtraction {
    data object None : TerminalReceiptFailureExtraction
    data class Found(
        val failure: TerminalReceiptApplicationFailure,
    ) : TerminalReceiptFailureExtraction
}

/** Typed poison evidence carried from receipt validation into the worker fence. */
internal sealed interface TerminalReceiptApplicationFailure {
    data class PersistenceExhausted(
        val operation: TerminalReceiptPersistenceOperation,
        val owner: DeliveryOwnerBareJid,
        val receipt: TerminalReceiptRef?,
        val attempts: Int,
    ) : TerminalReceiptApplicationFailure
    data class DiscoveryCorrupt(val owner: DeliveryOwnerBareJid, val reason: TerminalReceiptCorruption) : TerminalReceiptApplicationFailure
    data class DiscoveryOwnerFenced(val requested: DeliveryOwnerBareJid, val actual: DeliveryOwnerBareJid?) : TerminalReceiptApplicationFailure
    data class ClaimBusy(val result: TerminalReceiptClaimResult.Busy) : TerminalReceiptApplicationFailure
    data class ClaimMissing(val result: TerminalReceiptClaimResult.ReceiptMissing) : TerminalReceiptApplicationFailure
    data class ClaimReplaced(val result: TerminalReceiptClaimResult.ReceiptReplaced) : TerminalReceiptApplicationFailure
    data class ClaimOwnerFenced(val result: TerminalReceiptClaimResult.OwnerFenced) : TerminalReceiptApplicationFailure
    data class ClaimCorrupt(val result: TerminalReceiptClaimResult.Corrupt) : TerminalReceiptApplicationFailure
    data class AcknowledgeLeaseMismatch(val result: TerminalReceiptAcknowledgeResult.LeaseMismatch) : TerminalReceiptApplicationFailure
    data class AcknowledgeMissing(val result: TerminalReceiptAcknowledgeResult.ReceiptMissing) : TerminalReceiptApplicationFailure
    data class AcknowledgeReplaced(val result: TerminalReceiptAcknowledgeResult.ReceiptReplaced) : TerminalReceiptApplicationFailure
    data class AcknowledgeCorrupt(val result: TerminalReceiptAcknowledgeResult.Corrupt) : TerminalReceiptApplicationFailure
    data class CleanupUnresolved(
        val evidence: TerminalReceiptCleanupEvidence,
    ) : TerminalReceiptApplicationFailure
}

internal enum class TerminalReceiptPersistenceOperation {
    DISCOVERY,
    CLAIM,
    ACKNOWLEDGE,
    RELEASE,
}

internal sealed interface TerminalReceiptDiscovery {
    data class Pending(val receipt: TerminalReceipt, val ref: TerminalReceiptRef) : TerminalReceiptDiscovery
    data class AlreadyAcknowledged(val receipt: TerminalReceipt) : TerminalReceiptDiscovery
    data object None : TerminalReceiptDiscovery
    data class OwnerFenced(
        val requested: DeliveryOwnerBareJid,
        val actual: DeliveryOwnerBareJid?,
    ) : TerminalReceiptDiscovery
    data class Corrupt(val reason: TerminalReceiptCorruption) : TerminalReceiptDiscovery
}

internal sealed interface TerminalReceiptClaimResult {
    val journal: DeliveryJournal

    data class Claimed(
        override val journal: DeliveryJournal,
        val receipt: TerminalReceipt,
        val lease: TerminalReceiptLease,
        val effects: List<social.waddle.android.client.prefs.TerminalReceiptEffect>,
    ) : TerminalReceiptClaimResult

    data class AlreadyAcknowledged(
        override val journal: DeliveryJournal,
        val receipt: TerminalReceipt,
        val acknowledgedBy: TerminalReceiptClaimState.Claimed?,
    ) : TerminalReceiptClaimResult

    data class Busy(
        override val journal: DeliveryJournal,
        val requested: TerminalReceiptClaimRequest,
        val current: TerminalReceiptLease,
    ) : TerminalReceiptClaimResult

    data class ReceiptMissing(override val journal: DeliveryJournal, val requested: TerminalReceiptRef) : TerminalReceiptClaimResult
    data class ReceiptReplaced(
        override val journal: DeliveryJournal,
        val requested: TerminalReceiptRef,
        val actual: TerminalReceiptRef,
    ) : TerminalReceiptClaimResult

    data class OwnerFenced(
        override val journal: DeliveryJournal,
        val requested: DeliveryOwnerBareJid,
        val actual: DeliveryOwnerBareJid?,
    ) : TerminalReceiptClaimResult

    data class Corrupt(
        override val journal: DeliveryJournal,
        val ref: TerminalReceiptRef,
        val reason: TerminalReceiptCorruption,
    ) : TerminalReceiptClaimResult
}

internal sealed interface TerminalReceiptAcknowledgeResult {
    val journal: DeliveryJournal

    data class Acknowledged(override val journal: DeliveryJournal, val receipt: TerminalReceipt) : TerminalReceiptAcknowledgeResult
    data class AlreadyAcknowledged(
        override val journal: DeliveryJournal,
        val receipt: TerminalReceipt,
        val acknowledgedBy: TerminalReceiptClaimState.Claimed?,
    ) : TerminalReceiptAcknowledgeResult
    data class LeaseMismatch(
        override val journal: DeliveryJournal,
        val requested: TerminalReceiptLease,
        val current: TerminalReceiptClaimState.Claimed?,
    ) : TerminalReceiptAcknowledgeResult
    data class ReceiptMissing(override val journal: DeliveryJournal, val requested: TerminalReceiptRef) : TerminalReceiptAcknowledgeResult
    data class ReceiptReplaced(override val journal: DeliveryJournal, val requested: TerminalReceiptRef, val actual: TerminalReceiptRef) : TerminalReceiptAcknowledgeResult
    data class Corrupt(override val journal: DeliveryJournal, val ref: TerminalReceiptRef, val reason: TerminalReceiptCorruption) : TerminalReceiptAcknowledgeResult
}

internal sealed interface TerminalReceiptReleaseResult {
    val journal: DeliveryJournal

    data class Released(override val journal: DeliveryJournal, val lease: TerminalReceiptLease) : TerminalReceiptReleaseResult
    data class AlreadyAcknowledged(
        override val journal: DeliveryJournal,
        val receipt: TerminalReceipt,
        val acknowledgedBy: TerminalReceiptClaimState.Claimed?,
    ) : TerminalReceiptReleaseResult
    data class LeaseMismatch(
        override val journal: DeliveryJournal,
        val requested: TerminalReceiptLease,
        val current: TerminalReceiptClaimState.Claimed?,
    ) : TerminalReceiptReleaseResult
    data class ReceiptMissing(override val journal: DeliveryJournal, val requested: TerminalReceiptRef) : TerminalReceiptReleaseResult
    data class ReceiptReplaced(override val journal: DeliveryJournal, val requested: TerminalReceiptRef, val actual: TerminalReceiptRef) : TerminalReceiptReleaseResult
    data class Corrupt(override val journal: DeliveryJournal, val ref: TerminalReceiptRef, val reason: TerminalReceiptCorruption) : TerminalReceiptReleaseResult
}

internal fun DeliveryJournal.discoverTerminalReceipt(owner: DeliveryOwnerBareJid): TerminalReceiptDiscovery {
    if (activeOwnerBareJid != owner.value) {
        return TerminalReceiptDiscovery.OwnerFenced(owner, activeOwnerBareJid?.let(::DeliveryOwnerBareJid))
    }
    val bucket = owners[owner.value] ?: return TerminalReceiptDiscovery.None
    val receipt = bucket.terminalReceipt ?: return TerminalReceiptDiscovery.None
    val ref = TerminalReceiptRef(receipt.owner, receipt.attempt, receipt.id)
    val validation = bucket.validateReceiptApplication(owner, receipt, ref)
    if (validation != null) return TerminalReceiptDiscovery.Corrupt(validation)
    return when (receipt.state) {
        is TerminalReceiptState.Pending -> TerminalReceiptDiscovery.Pending(receipt, ref)
        is TerminalReceiptState.Acknowledged,
        TerminalReceiptState.PreAcknowledged,
        -> TerminalReceiptDiscovery.AlreadyAcknowledged(receipt)
    }
}

internal fun DeliveryJournal.claimTerminalReceipt(
    request: TerminalReceiptClaimRequest,
): TerminalReceiptClaimResult {
    val owner = request.ref.owner
    if (activeOwnerBareJid != owner.value) {
        return TerminalReceiptClaimResult.OwnerFenced(this, owner, activeOwnerBareJid?.let(::DeliveryOwnerBareJid))
    }
    val bucket = owners[owner.value] ?: return TerminalReceiptClaimResult.ReceiptMissing(this, request.ref)
    val receipt = bucket.terminalReceipt ?: return TerminalReceiptClaimResult.ReceiptMissing(this, request.ref)
    if (!receipt.matches(request.ref)) {
        return TerminalReceiptClaimResult.ReceiptReplaced(this, request.ref, receipt.ref())
    }
    val validation = bucket.validateReceiptApplication(owner, receipt, request.ref)
    if (validation != null) return TerminalReceiptClaimResult.Corrupt(this, request.ref, validation)
    val pending = receipt.state as? TerminalReceiptState.Pending
        ?: return TerminalReceiptClaimResult.AlreadyAcknowledged(
            this,
            receipt,
            (receipt.state as? TerminalReceiptState.Acknowledged)?.claim,
        )
    val nextClaim = request.claim
    val nextReceipt = when (val current = pending.claim) {
        TerminalReceiptClaimState.Unclaimed -> receipt.withClaim(nextClaim)
        is TerminalReceiptClaimState.Claimed -> when {
            current == nextClaim -> receipt
            current.processEpoch != nextClaim.processEpoch -> receipt.withClaim(nextClaim)
            else -> return TerminalReceiptClaimResult.Busy(
                this,
                request,
                TerminalReceiptLease(request.ref, current),
            )
        }
    }
    val nextJournal = withOwner(owner.value, bucket.copy(terminalReceipt = nextReceipt))
    return TerminalReceiptClaimResult.Claimed(
        journal = nextJournal,
        receipt = nextReceipt,
        lease = TerminalReceiptLease(request.ref, nextClaim),
        effects = (nextReceipt.state as TerminalReceiptState.Pending).effects,
    )
}

internal fun DeliveryJournal.acknowledgeTerminalReceipt(
    lease: TerminalReceiptLease,
): TerminalReceiptAcknowledgeResult {
    val resolved = resolveExactLease(lease)
    return when (resolved) {
        is ExactLeaseResolution.Pending -> {
            if (resolved.pending.claim != lease.claim) {
                TerminalReceiptAcknowledgeResult.LeaseMismatch(this, lease, resolved.pending.claim as? TerminalReceiptClaimState.Claimed)
            } else {
                val receipt = resolved.receipt.copy(state = TerminalReceiptState.Acknowledged(lease.claim))
                TerminalReceiptAcknowledgeResult.Acknowledged(
                    withOwner(lease.ref.owner.value, resolved.bucket.copy(terminalReceipt = receipt)),
                    receipt,
                )
            }
        }
        is ExactLeaseResolution.Acknowledged -> if (resolved.claim == lease.claim) {
            TerminalReceiptAcknowledgeResult.AlreadyAcknowledged(this, resolved.receipt, resolved.claim)
        } else {
            TerminalReceiptAcknowledgeResult.LeaseMismatch(this, lease, resolved.claim)
        }
        is ExactLeaseResolution.PreAcknowledged -> TerminalReceiptAcknowledgeResult.LeaseMismatch(this, lease, null)
        is ExactLeaseResolution.Missing -> TerminalReceiptAcknowledgeResult.ReceiptMissing(this, lease.ref)
        is ExactLeaseResolution.Replaced -> TerminalReceiptAcknowledgeResult.ReceiptReplaced(this, lease.ref, resolved.actual)
        is ExactLeaseResolution.Corrupt -> TerminalReceiptAcknowledgeResult.Corrupt(this, lease.ref, resolved.reason)
    }
}

internal fun DeliveryJournal.releaseTerminalReceipt(
    lease: TerminalReceiptLease,
): TerminalReceiptReleaseResult {
    val resolved = resolveExactLease(lease)
    return when (resolved) {
        is ExactLeaseResolution.Pending -> {
            if (resolved.pending.claim != lease.claim) {
                TerminalReceiptReleaseResult.LeaseMismatch(this, lease, resolved.pending.claim as? TerminalReceiptClaimState.Claimed)
            } else {
                val receipt = resolved.receipt.copy(
                    state = resolved.pending.copy(claim = TerminalReceiptClaimState.Unclaimed),
                )
                TerminalReceiptReleaseResult.Released(
                    withOwner(lease.ref.owner.value, resolved.bucket.copy(terminalReceipt = receipt)),
                    lease,
                )
            }
        }
        is ExactLeaseResolution.Acknowledged -> if (resolved.claim == lease.claim) {
            TerminalReceiptReleaseResult.AlreadyAcknowledged(this, resolved.receipt, resolved.claim)
        } else {
            TerminalReceiptReleaseResult.LeaseMismatch(this, lease, resolved.claim)
        }
        is ExactLeaseResolution.PreAcknowledged -> TerminalReceiptReleaseResult.LeaseMismatch(this, lease, null)
        is ExactLeaseResolution.Missing -> TerminalReceiptReleaseResult.ReceiptMissing(this, lease.ref)
        is ExactLeaseResolution.Replaced -> TerminalReceiptReleaseResult.ReceiptReplaced(this, lease.ref, resolved.actual)
        is ExactLeaseResolution.Corrupt -> TerminalReceiptReleaseResult.Corrupt(this, lease.ref, resolved.reason)
    }
}

private sealed interface ExactLeaseResolution {
    data class Pending(val bucket: DeliveryOwnerJournal, val receipt: TerminalReceipt, val pending: TerminalReceiptState.Pending) : ExactLeaseResolution
    data class Acknowledged(val receipt: TerminalReceipt, val claim: TerminalReceiptClaimState.Claimed) : ExactLeaseResolution
    data object PreAcknowledged : ExactLeaseResolution
    data object Missing : ExactLeaseResolution
    data class Replaced(val actual: TerminalReceiptRef) : ExactLeaseResolution
    data class Corrupt(val reason: TerminalReceiptCorruption) : ExactLeaseResolution
}

/** A committed lease is non-revocable by a later active-owner switch. */
private fun DeliveryJournal.resolveExactLease(lease: TerminalReceiptLease): ExactLeaseResolution {
    val bucket = owners[lease.ref.owner.value] ?: return ExactLeaseResolution.Missing
    val receipt = bucket.terminalReceipt ?: return ExactLeaseResolution.Missing
    if (!receipt.matches(lease.ref)) return ExactLeaseResolution.Replaced(receipt.ref())
    val validation = bucket.validateReceiptApplication(lease.ref.owner, receipt, lease.ref)
    if (validation != null) return ExactLeaseResolution.Corrupt(validation)
    return when (val state = receipt.state) {
        is TerminalReceiptState.Pending -> ExactLeaseResolution.Pending(bucket, receipt, state)
        is TerminalReceiptState.Acknowledged -> ExactLeaseResolution.Acknowledged(receipt, state.claim)
        TerminalReceiptState.PreAcknowledged -> ExactLeaseResolution.PreAcknowledged
    }
}

private fun TerminalReceipt.matches(ref: TerminalReceiptRef): Boolean =
    owner == ref.owner && attempt == ref.attempt && id == ref.id

private fun TerminalReceipt.ref(): TerminalReceiptRef = TerminalReceiptRef(owner, attempt, id)

private fun TerminalReceipt.withClaim(claim: TerminalReceiptClaimState.Claimed): TerminalReceipt = copy(
    state = (state as TerminalReceiptState.Pending).copy(claim = claim),
)

private fun DeliveryOwnerJournal.validateReceiptApplication(
    requestedOwner: DeliveryOwnerBareJid,
    receipt: TerminalReceipt,
    ref: TerminalReceiptRef,
): TerminalReceiptCorruption? {
    if (receipt.owner != requestedOwner || !receipt.matches(ref) || receipt.attempt.ownerBareJid != requestedOwner.value) {
        return TerminalReceiptCorruption.RECEIPT_BINDING_MISMATCH
    }
    if (activeAttempt != null) return TerminalReceiptCorruption.ACTIVE_ATTEMPT_REMAINS
    if (terminalIntents.isNotEmpty()) return TerminalReceiptCorruption.TERMINAL_INTENTS_REMAIN
    if (outboundRows.map { it.identity }.toSet().size != outboundRows.size) return TerminalReceiptCorruption.DUPLICATE_ROW
    if (outboundRows.any { it.identity.ownerBareJid != requestedOwner.value }) return TerminalReceiptCorruption.ROW_OWNER_MISMATCH
    if (outboundRows.any { it.ownership is OutboundOwnership.Terminal }) return TerminalReceiptCorruption.TERMINAL_ROW_REMAINS
    if (outboundRows.any { it.ownership is OutboundOwnership.NativeOwned }) return TerminalReceiptCorruption.NATIVE_OWNED_ROW_REMAINS
    val pending = receipt.state as? TerminalReceiptState.Pending ?: return null
    if (pending.effects.isEmpty()) return TerminalReceiptCorruption.EMPTY_EFFECTS
    if (pending.effects.map { it.callback }.toSet().size != pending.effects.size ||
        pending.effects.map { it.row.identity }.toSet().size != pending.effects.size
    ) return TerminalReceiptCorruption.DUPLICATE_EFFECT
    if (pending.effects.any { effect ->
            effect.row.identity.ownerBareJid != requestedOwner.value ||
                effect.callback.row != effect.row.identity ||
                effect.callback.attempt != receipt.attempt
        }
    ) return TerminalReceiptCorruption.EFFECT_BINDING_MISMATCH
    if (!pending.effects.zipWithNext().all { (first, second) -> first.row.sequence < second.row.sequence }) {
        return TerminalReceiptCorruption.REVERSED_EFFECT_ORDER
    }
    return null
}

internal suspend fun SessionPrefs.discoverTerminalReceipt(
    owner: DeliveryOwnerBareJid,
): TerminalReceiptDiscovery = updateDeliveryJournal { journal ->
    DeliveryJournalMutation(journal, journal.discoverTerminalReceipt(owner))
}

internal suspend fun SessionPrefs.claimTerminalReceipt(
    request: TerminalReceiptClaimRequest,
): TerminalReceiptClaimResult = updateDeliveryJournal { journal ->
    val result = journal.claimTerminalReceipt(request)
    DeliveryJournalMutation(result.journal, result)
}

internal suspend fun SessionPrefs.acknowledgeTerminalReceipt(
    lease: TerminalReceiptLease,
): TerminalReceiptAcknowledgeResult = updateDeliveryJournal { journal ->
    val result = journal.acknowledgeTerminalReceipt(lease)
    DeliveryJournalMutation(result.journal, result)
}

internal suspend fun SessionPrefs.releaseTerminalReceipt(
    lease: TerminalReceiptLease,
): TerminalReceiptReleaseResult = updateDeliveryJournal { journal ->
    val result = journal.releaseTerminalReceipt(lease)
    DeliveryJournalMutation(result.journal, result)
}
