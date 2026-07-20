package social.waddle.android.client

import social.waddle.android.client.DeliveryJournalStore.ResumeTransitionResult
import social.waddle.android.client.prefs.CommittedResumeTransition
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryAttemptTransition
import social.waddle.android.client.prefs.DeliveryJournal
import social.waddle.android.client.prefs.DeliveryJournalMutation
import social.waddle.android.client.prefs.DeliveryOwnerJournal
import social.waddle.android.client.prefs.NativeOutboundPhase
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.SmResumeSlot
import java.nio.ByteBuffer
import java.security.MessageDigest

private data class ResumeFailureCommit(
    val transition: DeliveryAttemptTransition,
    val affectedStanzaIds: Set<String>,
    val affectedSetDigest: String,
    val nowMillis: Long,
)

internal fun DeliveryJournal.commitResumeFailure(
    transition: DeliveryAttemptTransition,
    affectedStanzaIds: Set<String>,
    nowMillis: Long,
): DeliveryJournalMutation<ResumeTransitionResult> {
    if (!transition.isValidGenerationChange()) {
        return DeliveryJournalMutation(this, ResumeTransitionResult.InvalidTransition)
    }
    val command = ResumeFailureCommit(
        transition = transition,
        affectedStanzaIds = affectedStanzaIds,
        affectedSetDigest = affectedSetDigest(affectedStanzaIds),
        nowMillis = nowMillis,
    )
    val old = transition.old
    val owner = owners[old.ownerBareJid]
        ?: return DeliveryJournalMutation(this, ResumeTransitionResult.StaleAttempt)
    if (activeOwnerBareJid != old.ownerBareJid) {
        return DeliveryJournalMutation(this, ResumeTransitionResult.StaleAttempt)
    }
    owner.committedResult(command)?.let { result ->
        return DeliveryJournalMutation(this, result)
    }
    return when (val result = owner.commitUnseenResumeFailure(command)) {
        is OwnerCommitResult.Rejected -> DeliveryJournalMutation(this, result.result)
        is OwnerCommitResult.Updated -> DeliveryJournalMutation(
            journal = withOwner(old.ownerBareJid, result.owner),
            result = ResumeTransitionResult.Committed(result.smVersion),
        )
    }
}

private fun DeliveryAttemptTransition.isValidGenerationChange(): Boolean {
    val oldGeneration = old.nativeGeneration.value
    return old.ownerBareJid == fresh.ownerBareJid &&
        old.attemptId != fresh.attemptId &&
        oldGeneration != ULong.MAX_VALUE &&
        fresh.nativeGeneration.value == oldGeneration + 1u
}

private fun DeliveryOwnerJournal.committedResult(
    command: ResumeFailureCommit,
): ResumeTransitionResult? {
    val committed = resumeTransitionReceipts.firstOrNull {
        it.transition.old.attemptId == command.transition.old.attemptId
    } ?: return null
    return if (
        committed.transition == command.transition &&
        committed.affectedSetDigest == command.affectedSetDigest
    ) {
        ResumeTransitionResult.AlreadyCommitted(committed.smVersion)
    } else {
        ResumeTransitionResult.ReceiptConflict
    }
}

private fun DeliveryOwnerJournal.commitUnseenResumeFailure(
    command: ResumeFailureCommit,
): OwnerCommitResult {
    val old = command.transition.old
    if (activeAttempt != old) {
        return OwnerCommitResult.Rejected(ResumeTransitionResult.StaleAttempt)
    }
    val actualAffected = resumeRowsOwnedBy(old)
    if (actualAffected != command.affectedStanzaIds) {
        return OwnerCommitResult.Rejected(
            ResumeTransitionResult.AffectedSetMismatch(
                expected = actualAffected,
                actual = command.affectedStanzaIds,
            ),
        )
    }
    if (hasPendingResumeTerminal(old) || sm.version == Long.MAX_VALUE) {
        return OwnerCommitResult.Rejected(ResumeTransitionResult.StaleAttempt)
    }
    val retainedReceipts = gcTransitionReceipts(command.nowMillis).resumeTransitionReceipts
    if (retainedReceipts.size >= DeliveryJournalStore.MAX_TRANSITION_RECEIPTS_PER_OWNER) {
        return OwnerCommitResult.Rejected(ResumeTransitionResult.ReceiptCapacityExhausted)
    }

    val fresh = command.transition.fresh
    val rows = outboundRows.map { row ->
        val ownership = row.ownership as? OutboundOwnership.NativeOwned
        if (ownership?.attempt == old && ownership.phase == NativeOutboundPhase.RESUME) {
            row.copy(
                ownership = OutboundOwnership.NativeOwned(
                    attempt = fresh,
                    phase = NativeOutboundPhase.FRESH_FALLBACK,
                ),
            )
        } else {
            row
        }
    }
    val smVersion = sm.version + 1
    return OwnerCommitResult.Updated(
        owner = copy(
            activeAttempt = fresh,
            resumeTransitionReceipts = retainedReceipts + CommittedResumeTransition(
                transition = command.transition,
                affectedSetDigest = command.affectedSetDigest,
                smVersion = smVersion,
                committedAtMillis = command.nowMillis,
            ),
            sm = SmResumeSlot(
                version = smVersion,
                tombstoneVersion = smVersion,
                writerAttempt = fresh,
                snapshot = null,
            ),
            outboundRows = rows,
        ),
        smVersion = smVersion,
    )
}

private fun DeliveryOwnerJournal.resumeRowsOwnedBy(
    attempt: DeliveryAttemptRef,
): Set<String> = outboundRows.mapNotNullTo(mutableSetOf()) { row ->
    val ownership = row.ownership as? OutboundOwnership.NativeOwned
    row.clientStanzaId.takeIf {
        ownership?.attempt == attempt && ownership.phase == NativeOutboundPhase.RESUME
    }
}

private fun DeliveryOwnerJournal.hasPendingResumeTerminal(
    attempt: DeliveryAttemptRef,
): Boolean = terminalIntents.any { intent ->
    intent.expectedOwnership.attempt == attempt &&
        intent.expectedOwnership.phase == NativeOutboundPhase.RESUME
}

internal fun DeliveryOwnerJournal.gcTransitionReceipts(
    nowMillis: Long,
): DeliveryOwnerJournal {
    val retained = resumeTransitionReceipts.mapNotNull { receipt ->
        val old = receipt.transition.old
        val fresh = receipt.transition.fresh
        val referenced =
            activeAttempt == old ||
                activeAttempt == fresh ||
                sm.writerAttempt == old ||
                sm.writerAttempt == fresh ||
                outboundRows.any { row ->
                    val ownership = row.ownership as? OutboundOwnership.NativeOwned
                    ownership?.attempt == old || ownership?.attempt == fresh
                } ||
                terminalIntents.any { intent ->
                    intent.expectedOwnership.attempt == old ||
                        intent.expectedOwnership.attempt == fresh
                }
        when {
            referenced -> receipt
            receipt.terminalAtMillis == null ->
                receipt.copy(terminalAtMillis = nowMillis)
            nowMillis - receipt.terminalAtMillis >=
                DeliveryJournalStore.TRANSITION_RECEIPT_RETENTION_MILLIS -> null
            else -> receipt
        }
    }
    return if (retained == resumeTransitionReceipts) {
        this
    } else {
        copy(resumeTransitionReceipts = retained)
    }
}

private fun affectedSetDigest(stanzaIds: Set<String>): String {
    val digest = MessageDigest.getInstance("SHA-256")
    digest.update("waddle-resume-affected-v1".toByteArray(Charsets.UTF_8))
    digest.update(ByteBuffer.allocate(Int.SIZE_BYTES).putInt(stanzaIds.size).array())
    stanzaIds.sorted().forEach { stanzaId ->
        val bytes = stanzaId.toByteArray(Charsets.UTF_8)
        digest.update(ByteBuffer.allocate(Int.SIZE_BYTES).putInt(bytes.size).array())
        digest.update(bytes)
    }
    return digest.digest().joinToString(separator = "") { byte ->
        (byte.toInt() and 0xff).toString(16).padStart(2, '0')
    }
}

private sealed interface OwnerCommitResult {
    data class Updated(
        val owner: DeliveryOwnerJournal,
        val smVersion: Long,
    ) : OwnerCommitResult

    data class Rejected(val result: ResumeTransitionResult) : OwnerCommitResult
}
