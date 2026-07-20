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

/** Complete in-memory identity of one durable terminal receipt. */
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
}

internal enum class TerminalReceiptOperation {
    DISCOVERY,
    CLAIM,
    ACKNOWLEDGE,
    RELEASE,
    DISPATCH,
}

/** Typed poison evidence carried from receipt validation into the worker fence. */
internal data class TerminalReceiptApplicationFailure(
    val operation: TerminalReceiptOperation,
    val corruption: TerminalReceiptCorruption?,
)

internal sealed interface TerminalReceiptDiscovery {
    data class Pending(val receipt: TerminalReceipt, val ref: TerminalReceiptRef) : TerminalReceiptDiscovery
    data class AlreadyAcknowledged(val receipt: TerminalReceipt) : TerminalReceiptDiscovery
    data object None : TerminalReceiptDiscovery
    data object Stale : TerminalReceiptDiscovery
    data class Corrupt(val reason: TerminalReceiptCorruption) : TerminalReceiptDiscovery
}

internal sealed interface TerminalReceiptApplicationResult {
    val journal: DeliveryJournal

    data class Claimed(
        override val journal: DeliveryJournal,
        val receipt: TerminalReceipt,
        val lease: TerminalReceiptLease,
        val effects: List<social.waddle.android.client.prefs.TerminalReceiptEffect>,
    ) : TerminalReceiptApplicationResult

    data class AlreadyAcknowledged(
        override val journal: DeliveryJournal,
        val receipt: TerminalReceipt,
    ) : TerminalReceiptApplicationResult

    data class Busy(
        override val journal: DeliveryJournal,
        val lease: TerminalReceiptLease,
    ) : TerminalReceiptApplicationResult

    data class None(override val journal: DeliveryJournal) : TerminalReceiptApplicationResult

    data class Stale(override val journal: DeliveryJournal) : TerminalReceiptApplicationResult
    data class Released(override val journal: DeliveryJournal) : TerminalReceiptApplicationResult
    data class Acknowledged(
        override val journal: DeliveryJournal,
        val receipt: TerminalReceipt,
    ) : TerminalReceiptApplicationResult

    data class Corrupt(
        override val journal: DeliveryJournal,
        val reason: TerminalReceiptCorruption,
    ) : TerminalReceiptApplicationResult
}

internal fun DeliveryJournal.discoverTerminalReceipt(owner: DeliveryOwnerBareJid): TerminalReceiptDiscovery {
    if (activeOwnerBareJid != owner.value) return TerminalReceiptDiscovery.Stale
    val bucket = owners[owner.value] ?: return TerminalReceiptDiscovery.None
    val receipt = bucket.terminalReceipt ?: return TerminalReceiptDiscovery.None
    val ref = TerminalReceiptRef(receipt.owner, receipt.attempt, receipt.id)
    val validation = bucket.validateReceiptApplication(owner, receipt, ref)
    if (validation != null) return TerminalReceiptDiscovery.Corrupt(validation)
    return when (receipt.state) {
        is TerminalReceiptState.Pending -> TerminalReceiptDiscovery.Pending(receipt, ref)
        TerminalReceiptState.Acknowledged -> TerminalReceiptDiscovery.AlreadyAcknowledged(receipt)
    }
}

internal fun DeliveryJournal.claimTerminalReceipt(
    request: TerminalReceiptClaimRequest,
): TerminalReceiptApplicationResult {
    val owner = request.ref.owner
    if (activeOwnerBareJid != owner.value) return TerminalReceiptApplicationResult.Stale(this)
    val bucket = owners[owner.value] ?: return TerminalReceiptApplicationResult.None(this)
    val receipt = bucket.terminalReceipt ?: return TerminalReceiptApplicationResult.None(this)
    if (!receipt.matches(request.ref)) return TerminalReceiptApplicationResult.Stale(this)
    val validation = bucket.validateReceiptApplication(owner, receipt, request.ref)
    if (validation != null) return TerminalReceiptApplicationResult.Corrupt(this, validation)
    val pending = receipt.state as? TerminalReceiptState.Pending
        ?: return TerminalReceiptApplicationResult.AlreadyAcknowledged(this, receipt)
    val nextClaim = request.claim
    val nextReceipt = when (val current = pending.claim) {
        TerminalReceiptClaimState.Unclaimed -> receipt.withClaim(nextClaim)
        is TerminalReceiptClaimState.Claimed -> when {
            current == nextClaim -> receipt
            current.processEpoch != nextClaim.processEpoch -> receipt.withClaim(nextClaim)
            else -> return TerminalReceiptApplicationResult.Busy(
                this,
                TerminalReceiptLease(request.ref, current),
            )
        }
    }
    val nextJournal = withOwner(owner.value, bucket.copy(terminalReceipt = nextReceipt))
    return TerminalReceiptApplicationResult.Claimed(
        journal = nextJournal,
        receipt = nextReceipt,
        lease = TerminalReceiptLease(request.ref, nextClaim),
        effects = (nextReceipt.state as TerminalReceiptState.Pending).effects,
    )
}

internal fun DeliveryJournal.acknowledgeTerminalReceipt(
    lease: TerminalReceiptLease,
): TerminalReceiptApplicationResult = mutateExactLease(lease, acknowledge = true)

internal fun DeliveryJournal.releaseTerminalReceipt(
    lease: TerminalReceiptLease,
): TerminalReceiptApplicationResult = mutateExactLease(lease, acknowledge = false)

private fun DeliveryJournal.mutateExactLease(
    lease: TerminalReceiptLease,
    acknowledge: Boolean,
): TerminalReceiptApplicationResult {
    val owner = lease.ref.owner
    if (activeOwnerBareJid != owner.value) return TerminalReceiptApplicationResult.Stale(this)
    val bucket = owners[owner.value] ?: return TerminalReceiptApplicationResult.None(this)
    val receipt = bucket.terminalReceipt ?: return TerminalReceiptApplicationResult.None(this)
    if (!receipt.matches(lease.ref)) return TerminalReceiptApplicationResult.Stale(this)
    val validation = bucket.validateReceiptApplication(owner, receipt, lease.ref)
    if (validation != null) return TerminalReceiptApplicationResult.Corrupt(this, validation)
    val pending = receipt.state as? TerminalReceiptState.Pending
        ?: return TerminalReceiptApplicationResult.AlreadyAcknowledged(this, receipt)
    if (pending.claim != lease.claim) {
        val current = pending.claim as? TerminalReceiptClaimState.Claimed
            ?: return TerminalReceiptApplicationResult.Stale(this)
        return TerminalReceiptApplicationResult.Busy(this, TerminalReceiptLease(lease.ref, current))
    }
    val nextReceipt = if (acknowledge) {
        receipt.copy(state = TerminalReceiptState.Acknowledged)
    } else {
        receipt.copy(state = pending.copy(claim = TerminalReceiptClaimState.Unclaimed))
    }
    val nextJournal = withOwner(owner.value, bucket.copy(terminalReceipt = nextReceipt))
    return if (acknowledge) {
        TerminalReceiptApplicationResult.Acknowledged(nextJournal, nextReceipt)
    } else {
        TerminalReceiptApplicationResult.Released(nextJournal)
    }
}

private fun TerminalReceipt.matches(ref: TerminalReceiptRef): Boolean =
    owner == ref.owner && attempt == ref.attempt && id == ref.id

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
    return null
}

internal suspend fun SessionPrefs.discoverTerminalReceipt(
    owner: DeliveryOwnerBareJid,
): TerminalReceiptDiscovery = updateDeliveryJournal { journal ->
    DeliveryJournalMutation(journal, journal.discoverTerminalReceipt(owner))
}

internal suspend fun SessionPrefs.claimTerminalReceipt(
    request: TerminalReceiptClaimRequest,
): TerminalReceiptApplicationResult = updateDeliveryJournal { journal ->
    val result = journal.claimTerminalReceipt(request)
    DeliveryJournalMutation(result.journal, result)
}

internal suspend fun SessionPrefs.acknowledgeTerminalReceipt(
    lease: TerminalReceiptLease,
): TerminalReceiptApplicationResult = updateDeliveryJournal { journal ->
    val result = journal.acknowledgeTerminalReceipt(lease)
    DeliveryJournalMutation(result.journal, result)
}

internal suspend fun SessionPrefs.releaseTerminalReceipt(
    lease: TerminalReceiptLease,
): TerminalReceiptApplicationResult = updateDeliveryJournal { journal ->
    val result = journal.releaseTerminalReceipt(lease)
    DeliveryJournalMutation(result.journal, result)
}
