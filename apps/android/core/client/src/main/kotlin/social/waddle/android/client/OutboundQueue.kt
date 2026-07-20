package social.waddle.android.client

import kotlinx.coroutines.flow.first
import social.waddle.android.client.prefs.DeliveryAttemptId
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryAttemptTransition
import social.waddle.android.client.prefs.DeliveryCallbackRef
import social.waddle.android.client.prefs.DeliveryJournal
import social.waddle.android.client.prefs.DeliveryJournalMutation
import social.waddle.android.client.prefs.DeliveryOwnerJournal
import social.waddle.android.client.prefs.DeliveryRowIdentity
import social.waddle.android.client.prefs.DeliveryTerminalIntent
import social.waddle.android.client.prefs.DeliveryTerminalIntentId
import social.waddle.android.client.prefs.DeliveryTerminalKind
import social.waddle.android.client.prefs.NativeConnectionGeneration
import social.waddle.android.client.prefs.NativeOutboundPhase
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.QueuedOutboundDraft
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.SmResumeSlot
import social.waddle.android.client.prefs.SmResumeSnapshot

/**
 * Exact-CAS operations over the owner-scoped delivery journal.
 *
 * There is no stanza-ID-only mutation in this class. Every operation selects
 * one owner bucket, preserves foreign buckets, and compares the immutable row
 * identity plus the complete UUID attempt/native-generation ownership proof.
 */
class DeliveryJournalStore(
    private val sessionPrefs: SessionPrefs,
    private val capacityPerOwner: Int = DEFAULT_CAPACITY,
) {
    init {
        require(capacityPerOwner > 0) { "delivery capacity must be positive" }
    }

    sealed interface EnqueueResult {
        data class Stored(
            val row: QueuedOutboundMessage,
            val idempotent: Boolean,
        ) : EnqueueResult

        data class Conflict(
            val existing: DeliveryRowIdentity,
            val proposedDigest: social.waddle.android.client.prefs.DeliveryPayloadDigest,
        ) : EnqueueResult

        data object CapacityExhausted : EnqueueResult
        data object StaleAttempt : EnqueueResult
    }

    sealed interface LiveAdmissionResult {
        data class Claimed(
            val row: QueuedOutboundMessage,
        ) : LiveAdmissionResult

        data class Queued(
            val row: QueuedOutboundMessage,
            val blocker: QueuedOutboundMessage,
            val idempotent: Boolean,
        ) : LiveAdmissionResult

        data class Conflict(
            val existing: DeliveryRowIdentity,
            val proposedDigest: social.waddle.android.client.prefs.DeliveryPayloadDigest,
        ) : LiveAdmissionResult

        data object CapacityExhausted : LiveAdmissionResult

        data object StaleAttempt : LiveAdmissionResult
    }

    data class AttemptBootstrap(
        val attempt: DeliveryAttemptRef,
        val resumeSnapshot: SmResumeSnapshot?,
        val smVersion: Long,
    )

    /** Exact next-attempt gate for terminal receipt tombstones. */
    internal sealed interface BeginAttemptResult {
        data class Started(val bootstrap: AttemptBootstrap) : BeginAttemptResult

        data class PendingReceipt(
            val receipt: TerminalReceiptRef,
            val claim: social.waddle.android.client.prefs.TerminalReceiptClaimState,
        ) : BeginAttemptResult

        data class TombstoneNotPostFence(
            val receipt: TerminalReceiptRef,
            val state: social.waddle.android.client.prefs.TerminalReceiptState,
        ) : BeginAttemptResult
    }

    sealed interface TerminalRecordResult {
        data class Recorded(
            val intent: DeliveryTerminalIntent,
        ) : TerminalRecordResult

        data object Stale : TerminalRecordResult
    }

    sealed interface TerminalEffect {
        val callback: DeliveryCallbackRef
        val row: QueuedOutboundMessage

        data class Acknowledged(
            override val callback: DeliveryCallbackRef,
            override val row: QueuedOutboundMessage,
        ) : TerminalEffect

        data class Failed(
            override val callback: DeliveryCallbackRef,
            override val row: QueuedOutboundMessage,
        ) : TerminalEffect
    }

    sealed interface ResumeTransitionResult {
        data class Committed(val smVersion: Long) : ResumeTransitionResult

        data class AlreadyCommitted(val smVersion: Long) : ResumeTransitionResult

        data object InvalidTransition : ResumeTransitionResult

        data object StaleAttempt : ResumeTransitionResult

        data object ReceiptConflict : ResumeTransitionResult

        data object ReceiptCapacityExhausted : ResumeTransitionResult

        data class AffectedSetMismatch(
            val expected: Set<String>,
            val actual: Set<String>,
        ) : ResumeTransitionResult
    }

    /**
     * Atomically consume this owner's SM snapshot, rotate durable UUID
     * attempt identity, and reconcile only this owner's rows.
     */
    suspend fun beginAttempt(ownerBareJid: String): AttemptBootstrap = when (val result = beginAttemptResult(ownerBareJid)) {
        is BeginAttemptResult.Started -> result.bootstrap
        is BeginAttemptResult.PendingReceipt,
        is BeginAttemptResult.TombstoneNotPostFence,
        -> throw TerminalReceiptAttemptBlockedException(result)
    }

    internal suspend fun beginAttemptResult(ownerBareJid: String): BeginAttemptResult {
        val replacement = DeliveryAttemptRef(
            ownerBareJid = ownerBareJid,
            attemptId = DeliveryAttemptId.random(),
            nativeGeneration = NativeConnectionGeneration.initial(),
        )
        return sessionPrefs.updateDeliveryJournal { journal ->
            journal.beginDeliveryAttempt(
                ownerBareJid = ownerBareJid,
                replacement = replacement,
                nowMillis = System.currentTimeMillis(),
            )
        }
    }

    /**
     * Persist one monotonic SM callback for the exact active attempt.
     * A consumed/cleared version is a tombstone and stale callbacks cannot
     * resurrect it.
     */
    suspend fun saveSmResume(
        attempt: DeliveryAttemptRef,
        version: Long,
        snapshot: SmResumeSnapshot?,
    ): Boolean = sessionPrefs.updateDeliveryJournal { journal ->
        val owner = journal.owners[attempt.ownerBareJid]
            ?: return@updateDeliveryJournal DeliveryJournalMutation(journal, false)
        val staleWriter =
            journal.activeOwnerBareJid != attempt.ownerBareJid ||
                owner.activeAttempt != attempt ||
                version <= owner.sm.version
        if (staleWriter) {
            return@updateDeliveryJournal DeliveryJournalMutation(journal, false)
        }
        val nextSm = SmResumeSlot(
            version = version,
            tombstoneVersion = if (snapshot == null) version else owner.sm.tombstoneVersion,
            writerAttempt = attempt,
            snapshot = snapshot,
        )
        DeliveryJournalMutation(
            journal = journal.withOwner(
                attempt.ownerBareJid,
                owner.copy(sm = nextSm),
            ),
            result = true,
        )
    }

    /** Self-fence one exact attempt before logout or owner replacement. */
    suspend fun fenceAttempt(attempt: DeliveryAttemptRef): Boolean =
        sessionPrefs.updateDeliveryJournal { journal ->
            val owner = journal.owners[attempt.ownerBareJid]
            if (
                journal.activeOwnerBareJid != attempt.ownerBareJid ||
                owner?.activeAttempt != attempt
            ) {
                return@updateDeliveryJournal DeliveryJournalMutation(journal, false)
            }
            DeliveryJournalMutation(
                journal = journal.withOwner(
                    attempt.ownerBareJid,
                    owner.copy(activeAttempt = null),
                ),
                result = true,
            )
        }

    suspend fun activeAttempt(ownerBareJid: String): DeliveryAttemptRef? =
        sessionPrefs.deliveryJournal.first().let { journal ->
            if (journal.activeOwnerBareJid == ownerBareJid) {
                journal.owners[ownerBareJid]?.activeAttempt
            } else {
                null
            }
        }

    internal suspend fun discoverTerminalReceipt(
        owner: social.waddle.android.client.prefs.DeliveryOwnerBareJid,
    ): TerminalReceiptDiscovery = sessionPrefs.discoverTerminalReceipt(owner)

    internal suspend fun claimTerminalReceipt(
        request: TerminalReceiptClaimRequest,
    ): TerminalReceiptClaimResult = sessionPrefs.claimTerminalReceipt(request)

    internal suspend fun acknowledgeTerminalReceipt(
        lease: TerminalReceiptLease,
    ): TerminalReceiptAcknowledgeResult = sessionPrefs.acknowledgeTerminalReceipt(lease)

    internal suspend fun releaseTerminalReceipt(
        lease: TerminalReceiptLease,
    ): TerminalReceiptReleaseResult = sessionPrefs.releaseTerminalReceipt(lease)

    /**
     * Exact durability barrier for one Rust-minted failed-resume handoff.
     *
     * All RESUME rows and the owner SM tombstone move in one DataStore edit.
     * A duplicate of the already committed transition is idempotent; any
     * stale/out-of-order or affected-set mismatch fails closed.
     */
    suspend fun rotateAfterResumeFailure(
        transition: DeliveryAttemptTransition,
        affectedStanzaIds: Set<String>,
    ): ResumeTransitionResult = sessionPrefs.updateDeliveryJournal { journal ->
        journal.commitResumeFailure(
            transition = transition,
            affectedStanzaIds = affectedStanzaIds,
            nowMillis = System.currentTimeMillis(),
        )
    }

    suspend fun enqueueAndClaimAbsoluteHead(
        draft: QueuedOutboundDraft,
        attempt: DeliveryAttemptRef,
        phase: NativeOutboundPhase = NativeOutboundPhase.FRESH,
    ): LiveAdmissionResult = sessionPrefs.updateDeliveryJournal { journal ->
        journal.liveAdmission(draft, attempt, phase)
    }

    private fun DeliveryJournal.liveAdmission(
        draft: QueuedOutboundDraft,
        attempt: DeliveryAttemptRef,
        phase: NativeOutboundPhase,
    ): DeliveryJournalMutation<LiveAdmissionResult> {
        val ownerBareJid = draft.ownerBareJid
        val owner = owners[ownerBareJid] ?: DeliveryOwnerJournal()
        if (
            activeOwnerBareJid != ownerBareJid ||
            owner.activeAttempt != attempt ||
            attempt.ownerBareJid != ownerBareJid
        ) {
            return DeliveryJournalMutation(
                this,
                LiveAdmissionResult.StaleAttempt,
            )
        }
        val currentHead = owner.absoluteHeadOrThrow(ownerBareJid)
        val retry = owner.retryAdmission(draft, currentHead)
        if (retry != null) {
            return DeliveryJournalMutation(this, retry)
        }
        if (owner.outboundRows.size >= capacityPerOwner) {
            return DeliveryJournalMutation(
                this,
                LiveAdmissionResult.CapacityExhausted,
            )
        }
        return appendLiveAdmission(owner, draft, currentHead, attempt, phase)
    }

    private fun DeliveryOwnerJournal.retryAdmission(
        draft: QueuedOutboundDraft,
        currentHead: QueuedOutboundMessage?,
    ): LiveAdmissionResult? {
        val existing = outboundRows.firstOrNull {
            it.clientStanzaId == draft.clientStanzaId
        } ?: return null
        if (existing.payloadDigest != draft.payloadDigest) {
            return LiveAdmissionResult.Conflict(
                existing.identity,
                draft.payloadDigest,
            )
        }
        val blocker = currentHead
            ?: error("an existing delivery row requires an absolute head")
        return LiveAdmissionResult.Queued(
            row = existing,
            blocker = blocker,
            idempotent = true,
        )
    }

    private fun DeliveryJournal.appendLiveAdmission(
        owner: DeliveryOwnerJournal,
        draft: QueuedOutboundDraft,
        currentHead: QueuedOutboundMessage?,
        attempt: DeliveryAttemptRef,
        phase: NativeOutboundPhase,
    ): DeliveryJournalMutation<LiveAdmissionResult> {
        val ownerBareJid = draft.ownerBareJid
        val sequence = owner.allocateSequenceOrThrow(ownerBareJid)
        val ready = draft.persisted(
            sequence,
            OutboundOwnership.Ready,
        )
        val (admitted, result) = if (currentHead == null) {
            val claimed = ready.copy(
                ownership = OutboundOwnership.NativeOwned(attempt, phase),
            )
            claimed to LiveAdmissionResult.Claimed(claimed)
        } else {
            ready to LiveAdmissionResult.Queued(
                ready,
                currentHead,
                false,
            )
        }
        return DeliveryJournalMutation(
            journal = withOwner(
                ownerBareJid,
                owner.copy(
                    nextSequence = owner.nextSequence + 1,
                    outboundRows = owner.outboundRows + admitted,
                ),
            ),
            result = result,
        )
    }

    suspend fun enqueueReady(draft: QueuedOutboundDraft): EnqueueResult =
        enqueue(
            draft = draft,
        )

    private suspend fun enqueue(
        draft: QueuedOutboundDraft,
    ): EnqueueResult = sessionPrefs.updateDeliveryJournal { journal ->
        val ownerBareJid = draft.ownerBareJid
        val owner = journal.owners[ownerBareJid] ?: DeliveryOwnerJournal()
        if (journal.activeOwnerBareJid != ownerBareJid) {
            return@updateDeliveryJournal DeliveryJournalMutation(
                journal,
                EnqueueResult.StaleAttempt,
            )
        }
        owner.absoluteHeadOrThrow(ownerBareJid)
        val existing = owner.outboundRows.firstOrNull {
            it.ownerBareJid == ownerBareJid &&
                it.clientStanzaId == draft.clientStanzaId
        }
        if (existing != null) {
            val result = if (existing.payloadDigest == draft.payloadDigest) {
                EnqueueResult.Stored(existing, idempotent = true)
            } else {
                EnqueueResult.Conflict(existing.identity, draft.payloadDigest)
            }
            return@updateDeliveryJournal DeliveryJournalMutation(journal, result)
        }

        if (owner.outboundRows.size >= capacityPerOwner) {
            return@updateDeliveryJournal DeliveryJournalMutation(
                journal,
                EnqueueResult.CapacityExhausted,
            )
        }
        val sequence = owner.allocateSequenceOrThrow(ownerBareJid)
        val stored = draft.persisted(
            sequence,
            OutboundOwnership.Ready,
        )
        val nextOwner = owner.copy(
            nextSequence = owner.nextSequence + 1,
            outboundRows = owner.outboundRows + stored,
        )
        DeliveryJournalMutation(
            journal = journal.withOwner(ownerBareJid, nextOwner),
            result = EnqueueResult.Stored(
                row = stored,
                idempotent = false,
            ),
        )
    }

    suspend fun claimAbsoluteReadyHead(
        ownerBareJid: String,
        attempt: DeliveryAttemptRef,
        phase: NativeOutboundPhase = NativeOutboundPhase.FRESH,
    ): QueuedOutboundMessage? = sessionPrefs.updateDeliveryJournal { journal ->
        val owner = journal.owners[ownerBareJid]
        if (
            journal.activeOwnerBareJid != ownerBareJid ||
            owner?.activeAttempt != attempt ||
            attempt.ownerBareJid != ownerBareJid
        ) {
            return@updateDeliveryJournal DeliveryJournalMutation(journal, null)
        }
        val head = owner.absoluteHeadOrThrow(ownerBareJid)
        if (head?.ownership != OutboundOwnership.Ready) {
            return@updateDeliveryJournal DeliveryJournalMutation(journal, null)
        }
        val claimed = head.copy(
            ownership = OutboundOwnership.NativeOwned(attempt, phase),
        )
        val rows = owner.outboundRows.map { row ->
            if (row.identity == head.identity) claimed else row
        }
        DeliveryJournalMutation(
            journal = journal.withOwner(ownerBareJid, owner.copy(outboundRows = rows)),
            result = claimed,
        )
    }

    suspend fun release(
        identity: DeliveryRowIdentity,
        expected: OutboundOwnership.NativeOwned,
    ): Boolean = transitionToReady(identity, expected)

    /** Exact-CAS ownership renewal/transition. */
    private suspend fun transitionToReady(
        identity: DeliveryRowIdentity,
        expected: OutboundOwnership,
    ): Boolean = sessionPrefs.updateDeliveryJournal { journal ->
        val owner = journal.owners[identity.ownerBareJid]
            ?: return@updateDeliveryJournal DeliveryJournalMutation(journal, false)
        if (journal.activeOwnerBareJid != identity.ownerBareJid) {
            return@updateDeliveryJournal DeliveryJournalMutation(journal, false)
        }
        var changed = false
        val rows = owner.outboundRows.map { row ->
            if (row.identity == identity && row.ownership == expected) {
                changed = true
                row.copy(ownership = OutboundOwnership.Ready)
            } else {
                row
            }
        }
        DeliveryJournalMutation(
            journal = journal.withOwner(identity.ownerBareJid, owner.copy(outboundRows = rows)),
            result = changed,
        )
    }

    /**
     * Atomically mark the exact native-owned row terminal and append its
     * durable intent. Stale owner/attempt/incarnation/digest callbacks no-op.
     */
    suspend fun recordTerminal(
        ownerBareJid: String,
        clientStanzaId: String,
        attempt: DeliveryAttemptRef,
        kind: DeliveryTerminalKind,
    ): TerminalRecordResult = sessionPrefs.updateDeliveryJournal { journal ->
        val owner = journal.owners[ownerBareJid]
            ?: return@updateDeliveryJournal DeliveryJournalMutation(
                journal,
                TerminalRecordResult.Stale,
            )
        val staleCallback =
            journal.activeOwnerBareJid != ownerBareJid ||
                owner.activeAttempt != attempt ||
                attempt.ownerBareJid != ownerBareJid
        if (staleCallback) {
            return@updateDeliveryJournal DeliveryJournalMutation(
                journal,
                TerminalRecordResult.Stale,
            )
        }
        val rowIndex = owner.outboundRows.indexOfFirst { row ->
            val ownership = row.ownership as? OutboundOwnership.NativeOwned
            row.ownerBareJid == ownerBareJid &&
                row.clientStanzaId == clientStanzaId &&
                ownership?.attempt == attempt
        }
        if (rowIndex < 0) {
            return@updateDeliveryJournal DeliveryJournalMutation(
                journal,
                TerminalRecordResult.Stale,
            )
        }
        val row = owner.outboundRows[rowIndex]
        val expected = row.ownership as OutboundOwnership.NativeOwned
        val intent = DeliveryTerminalIntent(
            id = DeliveryTerminalIntentId.random(),
            row = row.identity,
            expectedOwnership = expected,
            kind = kind,
            createdAtMillis = System.currentTimeMillis(),
        )
        val rows = owner.outboundRows.toMutableList().also {
            it[rowIndex] = row.copy(ownership = OutboundOwnership.Terminal(intent.id))
        }
        DeliveryJournalMutation(
            journal = journal.withOwner(
                ownerBareJid,
                owner.copy(
                    outboundRows = rows,
                    terminalIntents = owner.terminalIntents + intent,
                ),
            ),
            result = TerminalRecordResult.Recorded(intent),
        )
    }

    /**
     * Apply at most one owner-scoped intent in the same edit that removes
     * it. Two workers may race; only the worker receiving a non-null effect
     * is allowed to emit UI/router state.
     */
    suspend fun applyNextTerminal(ownerBareJid: String): TerminalEffect? =
        sessionPrefs.updateDeliveryJournal { journal ->
            val owner = journal.owners[ownerBareJid]
                ?: return@updateDeliveryJournal DeliveryJournalMutation(journal, null)
            if (journal.activeOwnerBareJid != ownerBareJid) {
                return@updateDeliveryJournal DeliveryJournalMutation(journal, null)
            }
            val intent = owner.terminalIntents.firstOrNull()
                ?: return@updateDeliveryJournal DeliveryJournalMutation(journal, null)
            val rowIndex = owner.outboundRows.indexOfFirst { row ->
                row.identity == intent.row &&
                    row.ownership == OutboundOwnership.Terminal(intent.id)
            }
            if (rowIndex < 0) {
                val cleaned = owner.copy(
                    terminalIntents = owner.terminalIntents.filterNot { it.id == intent.id },
                )
                return@updateDeliveryJournal DeliveryJournalMutation(
                    journal.withOwner(ownerBareJid, cleaned),
                    null,
                )
            }

            val callback = DeliveryCallbackRef(intent.row, intent.expectedOwnership.attempt)
            val rows = owner.outboundRows.toMutableList()
            val terminalRow = rows[rowIndex]
            var nextOwner = owner
            val effect = when (intent.kind) {
                DeliveryTerminalKind.ACK -> {
                    rows.removeAt(rowIndex)
                    TerminalEffect.Acknowledged(callback, terminalRow)
                }
                DeliveryTerminalKind.NONRETRYABLE_DELETE -> {
                    rows.removeAt(rowIndex)
                    TerminalEffect.Failed(callback, terminalRow)
                }
                DeliveryTerminalKind.NATIVE_FAILURE -> {
                    // Failed-resume rows never reach this path: the native
                    // pull boundary subsumes their complete affected set in
                    // rotateAfterResumeFailure. A per-row failure is a
                    // terminal failure of the already-fresh attempt.
                    rows[rowIndex] = rows[rowIndex].copy(
                        ownership = OutboundOwnership.Ready,
                    )
                    TerminalEffect.Failed(callback, terminalRow)
                }
            }
            nextOwner = nextOwner.copy(
                outboundRows = rows,
                terminalIntents = owner.terminalIntents.filterNot { it.id == intent.id },
            ).gcTransitionReceipts(System.currentTimeMillis())
            DeliveryJournalMutation(
                journal = journal.withOwner(ownerBareJid, nextOwner),
                result = effect,
            )
        }

    suspend fun hasTerminalIntents(ownerBareJid: String): Boolean =
        sessionPrefs.deliveryJournal.first().let { journal ->
            journal.activeOwnerBareJid == ownerBareJid &&
                journal.owners[ownerBareJid]?.terminalIntents?.isNotEmpty() == true
        }

    suspend fun terminalIntentCount(ownerBareJid: String): Int =
        sessionPrefs.deliveryJournal.first().let { journal ->
            if (journal.activeOwnerBareJid == ownerBareJid) {
                journal.owners[ownerBareJid]?.terminalIntents?.size ?: 0
            } else {
                0
            }
        }

    suspend fun rows(ownerBareJid: String): List<QueuedOutboundMessage> =
        sessionPrefs.deliveryJournal.first()
            .owners[ownerBareJid]
            ?.outboundRows
            .orEmpty()

    /**
     * Lowest-row ordering: a native/terminal head blocks later Ready rows.
     * No work may bypass an uncertain or not-yet-applied predecessor.
     */
    suspend fun readyHead(ownerBareJid: String): QueuedOutboundMessage? =
        sessionPrefs.deliveryJournal.first()
            .owners[ownerBareJid]
            ?.absoluteHeadOrThrow(ownerBareJid)
            ?.takeIf { it.ownership == OutboundOwnership.Ready }

    companion object {
        const val DEFAULT_CAPACITY = 50
        const val MAX_TRANSITION_RECEIPTS_PER_OWNER = 256
        const val TRANSITION_RECEIPT_RETENTION_MILLIS = 8L * 24L * 60L * 60L * 1_000L
    }
}

internal class TerminalReceiptAttemptBlockedException(
    val result: DeliveryJournalStore.BeginAttemptResult,
) : IllegalStateException("terminal receipt blocks the next delivery attempt")
