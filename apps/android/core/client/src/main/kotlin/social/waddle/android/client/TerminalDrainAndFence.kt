package social.waddle.android.client

import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryCallbackRef
import social.waddle.android.client.prefs.DeliveryJournal
import social.waddle.android.client.prefs.DeliveryJournalMutation
import social.waddle.android.client.prefs.DeliveryOwnerBareJid
import social.waddle.android.client.prefs.DeliveryOwnerJournal
import social.waddle.android.client.prefs.DeliveryTerminalKind
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.ProcessEpoch
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.TerminalReceipt
import social.waddle.android.client.prefs.TerminalReceiptEffect
import social.waddle.android.client.prefs.TerminalReceiptId
import social.waddle.android.client.prefs.TerminalReceiptState

internal data class TerminalDrainAndFenceRequest(
    val attempt: DeliveryAttemptRef,
    val receiptId: TerminalReceiptId,
    val originProcessEpoch: ProcessEpoch,
    val nowMillis: Long,
    val maxEffects: Int,
) {
    init {
        require(nowMillis >= 0) { "terminal drain timestamp must be non-negative" }
        require(maxEffects >= 0) { "terminal drain effect bound must be non-negative" }
    }
}

internal enum class TerminalDrainAndFenceFailureReason {
    EFFECT_LIMIT_EXCEEDED,
    DUPLICATE_INTENT_ID,
    DUPLICATE_ROW_IDENTITY,
    INTENT_OWNER_MISMATCH,
    INTENT_ATTEMPT_MISMATCH,
    MISSING_INTENT_ROW,
    TERMINAL_INTENT_ID_MISMATCH,
    TERMINAL_OWNERSHIP_MISMATCH,
    ORPHAN_TERMINAL_ROW,
    ROW_OWNER_MISMATCH,
    NATIVE_OWNED_ROW_REMAINS,
    BLANK_REQUESTED_OWNER,
    BLANK_ACTIVE_OWNER,
    RECEIPT_CONFLICT,
}

internal sealed interface TerminalDrainAndFenceResult {
    val journal: DeliveryJournal

    data class Prepared(
        override val journal: DeliveryJournal,
        val receipt: TerminalReceipt,
    ) : TerminalDrainAndFenceResult

    data class AlreadyAcknowledged(
        override val journal: DeliveryJournal,
        val receipt: TerminalReceipt,
    ) : TerminalDrainAndFenceResult

    data class OwnershipMismatch(
        override val journal: DeliveryJournal,
        val requested: DeliveryAttemptRef,
        val actualOwner: DeliveryOwnerBareJid?,
        val actualAttempt: DeliveryAttemptRef?,
    ) : TerminalDrainAndFenceResult

    data class PriorReceiptPending(
        override val journal: DeliveryJournal,
        val receipt: TerminalReceipt,
    ) : TerminalDrainAndFenceResult

    data class Corrupt(
        override val journal: DeliveryJournal,
        val reason: TerminalDrainAndFenceFailureReason,
    ) : TerminalDrainAndFenceResult
}

/**
 * Pure TERMINAL_DRAIN_AND_FENCE preparation. R2 persists a returned journal
 * atomically; this function deliberately has no persistence or callback work.
 */
internal fun DeliveryJournal.prepareTerminalDrainAndFence(
    request: TerminalDrainAndFenceRequest,
): TerminalDrainAndFenceResult {
    terminalDrainOwnerValidation(request)?.let { return it }
    val requestedOwner = DeliveryOwnerBareJid(request.attempt.ownerBareJid)
    val owner = owners[requestedOwner.value]
    existingTerminalReceiptResult(owner, requestedOwner, request)?.let { return it }
    activeTerminalAttemptResult(owner, requestedOwner, request)?.let { return it }

    return prepareFencedTerminalDrain(checkNotNull(owner), requestedOwner, request)
}

private fun DeliveryJournal.terminalDrainOwnerValidation(
    request: TerminalDrainAndFenceRequest,
): TerminalDrainAndFenceResult.Corrupt? = when {
    request.attempt.ownerBareJid.isBlank() ->
        TerminalDrainAndFenceResult.Corrupt(
            this,
            TerminalDrainAndFenceFailureReason.BLANK_REQUESTED_OWNER,
        )
    activeOwnerBareJid?.isBlank() == true ->
        TerminalDrainAndFenceResult.Corrupt(
            this,
            TerminalDrainAndFenceFailureReason.BLANK_ACTIVE_OWNER,
        )
    else -> null
}

private fun DeliveryJournal.existingTerminalReceiptResult(
    owner: DeliveryOwnerJournal?,
    requestedOwner: DeliveryOwnerBareJid,
    request: TerminalDrainAndFenceRequest,
): TerminalDrainAndFenceResult? {
    val existing = owner?.terminalReceipt ?: return null
    val exactReceipt =
        existing.owner == requestedOwner &&
            existing.attempt == request.attempt &&
            existing.id == request.receiptId
    val fullyPostFence =
        owner.activeAttempt == null &&
            owner.terminalIntents.isEmpty() &&
            owner.outboundRows.none { it.ownership is OutboundOwnership.Terminal }
    if (!fullyPostFence || !exactReceipt) {
        return TerminalDrainAndFenceResult.Corrupt(
            this,
            TerminalDrainAndFenceFailureReason.RECEIPT_CONFLICT,
        )
    }
    owner.validatePostFenceRows(requestedOwner.value)?.let { reason ->
        return TerminalDrainAndFenceResult.Corrupt(this, reason)
    }
    return when (existing.state) {
        is TerminalReceiptState.Pending ->
            TerminalDrainAndFenceResult.PriorReceiptPending(this, existing)
        is TerminalReceiptState.Acknowledged,
        TerminalReceiptState.PreAcknowledged,
        -> TerminalDrainAndFenceResult.AlreadyAcknowledged(this, existing)
    }
}

private fun DeliveryJournal.activeTerminalAttemptResult(
    owner: DeliveryOwnerJournal?,
    requestedOwner: DeliveryOwnerBareJid,
    request: TerminalDrainAndFenceRequest,
): TerminalDrainAndFenceResult? {
    if (activeOwnerBareJid != requestedOwner.value) {
        return TerminalDrainAndFenceResult.OwnershipMismatch(
            journal = this,
            requested = request.attempt,
            actualOwner = activeOwnerBareJid?.let(::DeliveryOwnerBareJid),
            actualAttempt = activeOwnerBareJid?.let(owners::get)?.activeAttempt,
        )
    }
    val activeOwner = checkNotNull(owner)
    if (activeOwner.activeAttempt != request.attempt) {
        return TerminalDrainAndFenceResult.OwnershipMismatch(
            journal = this,
            requested = request.attempt,
            actualOwner = activeOwnerBareJid?.let(::DeliveryOwnerBareJid),
            actualAttempt = activeOwnerBareJid?.let(owners::get)?.activeAttempt,
        )
    }
    activeOwner.validateTerminalDrain(request)?.let { reason ->
        return TerminalDrainAndFenceResult.Corrupt(this, reason)
    }
    return null
}

private fun DeliveryJournal.prepareFencedTerminalDrain(
    owner: DeliveryOwnerJournal,
    requestedOwner: DeliveryOwnerBareJid,
    request: TerminalDrainAndFenceRequest,
): TerminalDrainAndFenceResult.Prepared {
    val effects = owner.terminalIntents.map { intent ->
        val row = owner.outboundRows.single { it.identity == intent.row }
        val callback = DeliveryCallbackRef(intent.row, request.attempt)
        when (intent.kind) {
            DeliveryTerminalKind.ACK -> TerminalReceiptEffect.Acknowledged(callback, row)
            DeliveryTerminalKind.NONRETRYABLE_DELETE,
            DeliveryTerminalKind.NATIVE_FAILURE,
            -> TerminalReceiptEffect.Failed(callback, row)
        }
    }
    val receipt = TerminalReceipt(
        owner = requestedOwner,
        attempt = request.attempt,
        id = request.receiptId,
        originProcessEpoch = request.originProcessEpoch,
        preparedAtMillis = request.nowMillis,
        state = if (effects.isEmpty()) {
            TerminalReceiptState.PreAcknowledged
        } else {
            TerminalReceiptState.Pending(
                claim = social.waddle.android.client.prefs.TerminalReceiptClaimState.Unclaimed,
                effects = effects,
            )
        },
    )
    val remainingRows = owner.outboundRows.mapNotNull { row ->
        val intent = owner.terminalIntents.singleOrNull { it.row == row.identity }
        when (intent?.kind) {
            null -> row
            DeliveryTerminalKind.ACK,
            DeliveryTerminalKind.NONRETRYABLE_DELETE,
            -> null
            DeliveryTerminalKind.NATIVE_FAILURE -> row.copy(ownership = OutboundOwnership.Ready)
        }
    }
    val nextOwner = owner.copy(
        activeAttempt = null,
        outboundRows = remainingRows,
        terminalIntents = emptyList(),
        terminalReceipt = receipt,
    )
    return TerminalDrainAndFenceResult.Prepared(
        journal = withOwner(requestedOwner.value, nextOwner),
        receipt = receipt,
    )
}

/**
 * Persists the pure TERMINAL_DRAIN_AND_FENCE outcome through the one serialized
 * delivery-journal transaction. The result and journal therefore describe the
 * same committed state.
 */
internal suspend fun SessionPrefs.persistTerminalDrainAndFence(
    request: TerminalDrainAndFenceRequest,
): TerminalDrainAndFenceResult = updateDeliveryJournal { journal ->
    val outcome = journal.prepareTerminalDrainAndFence(request)
    DeliveryJournalMutation(outcome.journal, outcome)
}

private fun DeliveryOwnerJournal.validateTerminalDrain(
    request: TerminalDrainAndFenceRequest,
): TerminalDrainAndFenceFailureReason? =
    terminalDrainJournalFailure(request)
        ?: terminalDrainIntentFailure(request)
        ?: terminalDrainOrphanFailure()

private fun DeliveryOwnerJournal.terminalDrainJournalFailure(
    request: TerminalDrainAndFenceRequest,
): TerminalDrainAndFenceFailureReason? {
    if (terminalIntents.size > request.maxEffects) {
        return TerminalDrainAndFenceFailureReason.EFFECT_LIMIT_EXCEEDED
    }
    if (terminalIntents.map { it.id }.toSet().size != terminalIntents.size) {
        return TerminalDrainAndFenceFailureReason.DUPLICATE_INTENT_ID
    }
    if (terminalIntents.map { it.row }.toSet().size != terminalIntents.size) {
        return TerminalDrainAndFenceFailureReason.DUPLICATE_ROW_IDENTITY
    }
    if (outboundRows.map { it.identity }.toSet().size != outboundRows.size) {
        return TerminalDrainAndFenceFailureReason.DUPLICATE_ROW_IDENTITY
    }
    if (outboundRows.any { it.identity.ownerBareJid != request.attempt.ownerBareJid }) {
        return TerminalDrainAndFenceFailureReason.ROW_OWNER_MISMATCH
    }
    if (outboundRows.any { it.ownership is OutboundOwnership.NativeOwned }) {
        return TerminalDrainAndFenceFailureReason.NATIVE_OWNED_ROW_REMAINS
    }
    return null
}

private fun DeliveryOwnerJournal.terminalDrainIntentFailure(
    request: TerminalDrainAndFenceRequest,
): TerminalDrainAndFenceFailureReason? {
    for (intent in terminalIntents) {
        if (intent.row.ownerBareJid != request.attempt.ownerBareJid) {
            return TerminalDrainAndFenceFailureReason.INTENT_OWNER_MISMATCH
        }
        if (intent.expectedOwnership.attempt != request.attempt) {
            return TerminalDrainAndFenceFailureReason.INTENT_ATTEMPT_MISMATCH
        }
        val rows = outboundRows.filter { it.identity == intent.row }
        if (rows.size != 1) return TerminalDrainAndFenceFailureReason.MISSING_INTENT_ROW
        when (val ownership = rows.single().ownership) {
            is OutboundOwnership.Terminal -> if (ownership.intentId != intent.id) {
                return TerminalDrainAndFenceFailureReason.TERMINAL_INTENT_ID_MISMATCH
            }
            else -> return TerminalDrainAndFenceFailureReason.TERMINAL_OWNERSHIP_MISMATCH
        }
    }
    return null
}

private fun DeliveryOwnerJournal.terminalDrainOrphanFailure(): TerminalDrainAndFenceFailureReason? {
    val intentIdentities = terminalIntents.map { it.row }.toSet()
    if (outboundRows.any { row ->
            row.ownership is OutboundOwnership.Terminal && row.identity !in intentIdentities
        }
    ) {
        return TerminalDrainAndFenceFailureReason.ORPHAN_TERMINAL_ROW
    }
    return null
}

private fun DeliveryOwnerJournal.validatePostFenceRows(
    ownerBareJid: String,
): TerminalDrainAndFenceFailureReason? {
    if (outboundRows.map { it.identity }.toSet().size != outboundRows.size) {
        return TerminalDrainAndFenceFailureReason.DUPLICATE_ROW_IDENTITY
    }
    if (outboundRows.any { it.identity.ownerBareJid != ownerBareJid }) {
        return TerminalDrainAndFenceFailureReason.ROW_OWNER_MISMATCH
    }
    if (outboundRows.any { it.ownership is OutboundOwnership.NativeOwned }) {
        return TerminalDrainAndFenceFailureReason.NATIVE_OWNED_ROW_REMAINS
    }
    return null
}
