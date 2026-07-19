package social.waddle.android.client

import java.util.UUID
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.DeliveryAttemptId
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryJournal
import social.waddle.android.client.prefs.DeliveryOwnerJournal
import social.waddle.android.client.prefs.DeliveryTerminalIntent
import social.waddle.android.client.prefs.DeliveryTerminalIntentId
import social.waddle.android.client.prefs.DeliveryTerminalKind
import social.waddle.android.client.prefs.NativeConnectionGeneration
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.ProcessEpoch
import social.waddle.android.client.prefs.QueuedOutboundContent
import social.waddle.android.client.prefs.QueuedOutboundDraft
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.QueuedOutboundPayload
import social.waddle.android.client.prefs.QueuedOutboundTarget
import social.waddle.android.client.prefs.DeliveryOwnerBareJid
import social.waddle.android.client.prefs.TerminalReceipt
import social.waddle.android.client.prefs.TerminalReceiptClaimState
import social.waddle.android.client.prefs.TerminalReceiptId
import social.waddle.android.client.prefs.TerminalReceiptState

class TerminalDrainAndFenceTest {
    @Test
    fun `zero terminal work fences exact attempt with acknowledged tombstone and is idempotent`() {
        val attempt = attempt("active")
        val request = request(attempt, "receipt")
        val before = journal(attempt)

        val prepared = before.prepareTerminalDrainAndFence(request) as TerminalDrainAndFenceResult.Prepared
        assertEquals(TerminalReceiptState.Acknowledged, prepared.receipt.state)
        assertEquals(null, prepared.journal.owners[OWNER]?.activeAttempt)
        assertEquals(prepared.receipt, prepared.journal.owners[OWNER]?.terminalReceipt)

        val repeated = prepared.journal.prepareTerminalDrainAndFence(request)
        assertEquals(
            TerminalDrainAndFenceResult.AlreadyAcknowledged(prepared.journal, prepared.receipt),
            repeated,
        )
        assertEquals(
            TerminalDrainAndFenceResult.Corrupt(
                prepared.journal,
                TerminalDrainAndFenceFailureReason.RECEIPT_CONFLICT,
            ),
            prepared.journal.prepareTerminalDrainAndFence(request(attempt, "different-receipt")),
        )
    }

    @Test
    fun `pending receipt remains authoritative after exact post-fence retry`() {
        val attempt = attempt("active")
        val intentId = intentId("pending")
        val row = row("pending", 1, intentId)
        val intent = intent(intentId, row, attempt, DeliveryTerminalKind.ACK)
        val first = journal(attempt, listOf(row), listOf(intent))
            .prepareTerminalDrainAndFence(request(attempt, "receipt")) as TerminalDrainAndFenceResult.Prepared

        val repeated = first.journal.prepareTerminalDrainAndFence(request(attempt, "receipt"))
        assertEquals(TerminalDrainAndFenceResult.PriorReceiptPending(first.journal, first.receipt), repeated)
        assertEquals(
            TerminalDrainAndFenceResult.Corrupt(
                first.journal,
                TerminalDrainAndFenceFailureReason.RECEIPT_CONFLICT,
            ),
            first.journal.prepareTerminalDrainAndFence(request(attempt, "different-receipt")),
        )
    }

    @Test
    fun `exact receipt retries validate every remaining post-fence row`() {
        val attempt = attempt("active")
        val acknowledged = journal(attempt)
            .prepareTerminalDrainAndFence(request(attempt, "ack")) as TerminalDrainAndFenceResult.Prepared
        val ready = row("ready", 1, intentId("ready")).copy(ownership = OutboundOwnership.Ready)
        val acknowledgedWithReady = acknowledged.journal.withRows(listOf(ready))
        assertEquals(
            TerminalDrainAndFenceResult.AlreadyAcknowledged(acknowledgedWithReady, acknowledged.receipt),
            acknowledgedWithReady.prepareTerminalDrainAndFence(request(attempt, "ack")),
        )
        assertEquals(
            TerminalDrainAndFenceResult.Corrupt(
                acknowledged.journal.withRows(
                    listOf(
                        ready.copy(
                            ownership = OutboundOwnership.NativeOwned(
                                attempt,
                                social.waddle.android.client.prefs.NativeOutboundPhase.FRESH,
                            ),
                        ),
                    ),
                ),
                TerminalDrainAndFenceFailureReason.NATIVE_OWNED_ROW_REMAINS,
            ),
            acknowledged.journal.withRows(
                listOf(
                    ready.copy(
                        ownership = OutboundOwnership.NativeOwned(
                            attempt,
                            social.waddle.android.client.prefs.NativeOutboundPhase.FRESH,
                        ),
                    ),
                ),
            ).prepareTerminalDrainAndFence(request(attempt, "ack")),
        )
        val duplicateRows = acknowledged.journal.withRows(listOf(ready, ready.copy(sequence = 2)))
        assertEquals(
            TerminalDrainAndFenceResult.Corrupt(
                duplicateRows,
                TerminalDrainAndFenceFailureReason.DUPLICATE_ROW_IDENTITY,
            ),
            duplicateRows.prepareTerminalDrainAndFence(request(attempt, "ack")),
        )

        val intentId = intentId("pending")
        val terminal = row("pending", 1, intentId)
        val pending = journal(attempt, listOf(terminal), listOf(intent(intentId, terminal, attempt, DeliveryTerminalKind.NATIVE_FAILURE)))
            .prepareTerminalDrainAndFence(request(attempt, "pending")) as TerminalDrainAndFenceResult.Prepared
        val foreignReady = checkNotNull(pending.journal.owners[OWNER])
            .outboundRows.single()
            .copy(ownerBareJid = OTHER_OWNER)
        val pendingWithForeignReady = pending.journal.withRows(listOf(foreignReady))
        assertEquals(
            TerminalDrainAndFenceResult.Corrupt(
                pendingWithForeignReady,
                TerminalDrainAndFenceFailureReason.ROW_OWNER_MISMATCH,
            ),
            pendingWithForeignReady.prepareTerminalDrainAndFence(request(attempt, "pending")),
        )
        assertEquals(
            TerminalDrainAndFenceResult.PriorReceiptPending(pending.journal, pending.receipt),
            pending.journal.prepareTerminalDrainAndFence(request(attempt, "pending")),
        )
    }

    @Test
    fun `same receipt id with a different exact attempt is corrupt`() {
        val active = attempt("active")
        val fenced = journal(active)
            .prepareTerminalDrainAndFence(request(active, "receipt")) as TerminalDrainAndFenceResult.Prepared
        val differentAttempt = attempt("different")
        assertEquals(
            TerminalDrainAndFenceResult.Corrupt(
                fenced.journal,
                TerminalDrainAndFenceFailureReason.RECEIPT_CONFLICT,
            ),
            fenced.journal.prepareTerminalDrainAndFence(request(differentAttempt, "receipt")),
        )
    }

    @Test
    fun `blank requested and active owners are typed corruption`() {
        val active = attempt("active")
        assertEquals(
            TerminalDrainAndFenceResult.Corrupt(
                journal(active),
                TerminalDrainAndFenceFailureReason.BLANK_REQUESTED_OWNER,
            ),
            journal(active).prepareTerminalDrainAndFence(request(active.copy(ownerBareJid = ""), "receipt")),
        )
        val blankActive = journal(active).copy(activeOwnerBareJid = "")
        assertEquals(
            TerminalDrainAndFenceResult.Corrupt(blankActive, TerminalDrainAndFenceFailureReason.BLANK_ACTIVE_OWNER),
            blankActive.prepareTerminalDrainAndFence(request(active, "receipt")),
        )
    }

    @Test
    fun `existing receipt is retry evidence only after every terminal fence artifact is gone`() {
        val attempt = attempt("active")
        val receipt = acknowledgedReceipt(attempt, "receipt")
        val before = journal(attempt).copy(
            owners = mapOf(OWNER to DeliveryOwnerJournal(activeAttempt = attempt, terminalReceipt = receipt)),
        )
        assertReceiptConflict(before, attempt)

        val intentId = intentId("residual")
        val terminalRow = row("residual", 1, intentId)
        val terminalIntent = intent(intentId, terminalRow, attempt, DeliveryTerminalKind.ACK)
        val residualIntent = before.copy(
            owners = mapOf(
                OWNER to DeliveryOwnerJournal(
                    terminalIntents = listOf(terminalIntent),
                    terminalReceipt = receipt,
                ),
            ),
        )
        assertReceiptConflict(residualIntent, attempt)

        val residualTerminal = before.copy(
            owners = mapOf(
                OWNER to DeliveryOwnerJournal(
                    outboundRows = listOf(terminalRow),
                    terminalReceipt = receipt,
                ),
            ),
        )
        assertReceiptConflict(residualTerminal, attempt)
    }

    private fun assertReceiptConflict(journal: DeliveryJournal, attempt: DeliveryAttemptRef) {
        assertEquals(
            TerminalDrainAndFenceResult.Corrupt(journal, TerminalDrainAndFenceFailureReason.RECEIPT_CONFLICT),
            journal.prepareTerminalDrainAndFence(request(attempt, "receipt")),
        )
    }

    @Test
    fun `mixed terminal drain preserves intent order and exact effects`() {
        val attempt = attempt("active")
        val ackId = intentId("ack")
        val nativeId = intentId("native")
        val deleteId = intentId("delete")
        val ack = row("ack", 1, ackId)
        val native = row("native", 2, nativeId)
        val delete = row("delete", 3, deleteId)
        val ready = row("ready", 4, intentId("ready")).copy(ownership = OutboundOwnership.Ready)
        val prepared = journal(
            attempt,
            rows = listOf(ack, native, delete, ready),
            intents = listOf(
                intent(ackId, ack, attempt, DeliveryTerminalKind.ACK),
                intent(nativeId, native, attempt, DeliveryTerminalKind.NATIVE_FAILURE),
                intent(deleteId, delete, attempt, DeliveryTerminalKind.NONRETRYABLE_DELETE),
            ),
        ).prepareTerminalDrainAndFence(request(attempt, "receipt")) as TerminalDrainAndFenceResult.Prepared

        val pending = prepared.receipt.state as TerminalReceiptState.Pending
        assertEquals(
            listOf(
                social.waddle.android.client.prefs.TerminalReceiptEffect.Acknowledged(
                    social.waddle.android.client.prefs.DeliveryCallbackRef(ack.identity, attempt),
                    ack,
                ),
                social.waddle.android.client.prefs.TerminalReceiptEffect.Failed(
                    social.waddle.android.client.prefs.DeliveryCallbackRef(native.identity, attempt),
                    native,
                ),
                social.waddle.android.client.prefs.TerminalReceiptEffect.Failed(
                    social.waddle.android.client.prefs.DeliveryCallbackRef(delete.identity, attempt),
                    delete,
                ),
            ),
            pending.effects,
        )
        assertTrue(pending.effects[0] is social.waddle.android.client.prefs.TerminalReceiptEffect.Acknowledged)
        assertTrue(pending.effects[1] is social.waddle.android.client.prefs.TerminalReceiptEffect.Failed)
        assertTrue(pending.effects[2] is social.waddle.android.client.prefs.TerminalReceiptEffect.Failed)
        assertEquals(
            listOf(native.copy(ownership = OutboundOwnership.Ready), ready),
            prepared.journal.owners[OWNER]?.outboundRows,
        )
        assertEquals(emptyList<DeliveryTerminalIntent>(), prepared.journal.owners[OWNER]?.terminalIntents)
        assertEquals(null, prepared.journal.owners[OWNER]?.activeAttempt)
        assertEquals(TerminalReceiptClaimState.Unclaimed, pending.claim)
    }

    @Test
    fun `foreign owners are preserved byte for byte`() {
        val attempt = attempt("active")
        val foreignAttempt = attempt("foreign", OTHER_OWNER)
        val foreign = DeliveryOwnerJournal(activeAttempt = foreignAttempt)
        val before = journal(attempt).copy(owners = journal(attempt).owners + (OTHER_OWNER to foreign))

        val prepared = before.prepareTerminalDrainAndFence(request(attempt, "receipt")) as TerminalDrainAndFenceResult.Prepared
        assertEquals(foreign, prepared.journal.owners[OTHER_OWNER])
    }

    @Test
    fun `owner and exact attempt mismatches preserve the journal`() {
        val active = attempt("active")
        val wrong = attempt("wrong")
        val wrongOwner = journal(active).copy(activeOwnerBareJid = OTHER_OWNER)
        assertMismatchUnchanged(wrongOwner, request(active, "receipt"))
        assertMismatchUnchanged(journal(active), request(wrong, "receipt"))
        assertMismatchUnchanged(
            journal(active).copy(
                owners = mapOf(OWNER to DeliveryOwnerJournal(activeAttempt = active.copy(ownerBareJid = OTHER_OWNER))),
            ),
            request(active, "receipt"),
        )
    }

    @Test
    fun `corrupt terminal state fails closed`() {
        val active = attempt("active")
        val id = intentId("one")
        val row = row("one", 1, id)
        val valid = intent(id, row, active, DeliveryTerminalKind.ACK)
        val duplicateId = intent(id, row("two", 2, id), active, DeliveryTerminalKind.ACK)
        val duplicateIntentRow = intent(intentId("second"), row, active, DeliveryTerminalKind.ACK)
        val duplicateRow = row.copy(sequence = 2)
        val wrongIntentId = row.copy(ownership = OutboundOwnership.Terminal(intentId("other")))
        val wrongAttempt = intent(id, row, attempt("other"), DeliveryTerminalKind.ACK)
        val foreignRow = row.copy(ownerBareJid = OTHER_OWNER)
        val wrongOwner = intent(
            id,
            foreignRow,
            active,
            DeliveryTerminalKind.ACK,
        )
        val missing = intent(id, row("missing", 1, id), active, DeliveryTerminalKind.ACK)
        val orphan = row("orphan", 1, id)

        assertCorrupt(journal(active, emptyList(), listOf(missing)), active, TerminalDrainAndFenceFailureReason.MISSING_INTENT_ROW)
        assertCorrupt(journal(active, listOf(row, duplicateIdRow(id)), listOf(valid, duplicateId)), active, TerminalDrainAndFenceFailureReason.DUPLICATE_INTENT_ID)
        assertCorrupt(journal(active, listOf(row), listOf(valid, duplicateIntentRow)), active, TerminalDrainAndFenceFailureReason.DUPLICATE_ROW_IDENTITY)
        assertCorrupt(journal(active, listOf(row, duplicateRow), listOf(valid)), active, TerminalDrainAndFenceFailureReason.DUPLICATE_ROW_IDENTITY)
        assertCorrupt(journal(active, listOf(wrongIntentId), listOf(valid)), active, TerminalDrainAndFenceFailureReason.TERMINAL_INTENT_ID_MISMATCH)
        assertCorrupt(journal(active, listOf(row), listOf(wrongAttempt)), active, TerminalDrainAndFenceFailureReason.INTENT_ATTEMPT_MISMATCH)
        assertCorrupt(journal(active, listOf(foreignRow), listOf(wrongOwner)), active, TerminalDrainAndFenceFailureReason.ROW_OWNER_MISMATCH)
        assertCorrupt(journal(active, listOf(orphan), emptyList()), active, TerminalDrainAndFenceFailureReason.ORPHAN_TERMINAL_ROW)
        assertCorrupt(
            journal(active, listOf(row.copy(ownership = OutboundOwnership.Ready)), listOf(valid)),
            active,
            TerminalDrainAndFenceFailureReason.TERMINAL_OWNERSHIP_MISMATCH,
        )
        assertCorrupt(
            journal(
                active,
                listOf(
                    row.copy(
                        ownership = OutboundOwnership.NativeOwned(
                            active,
                            social.waddle.android.client.prefs.NativeOutboundPhase.FRESH,
                        ),
                    ),
                ),
            ),
            active,
            TerminalDrainAndFenceFailureReason.NATIVE_OWNED_ROW_REMAINS,
        )
        assertCorrupt(journal(active, listOf(row), listOf(valid)), active, TerminalDrainAndFenceFailureReason.EFFECT_LIMIT_EXCEEDED, maxEffects = 0)

        val secondId = intentId("bounded-two")
        val secondRow = row("bounded-two", 2, secondId)
        val secondIntent = intent(secondId, secondRow, active, DeliveryTerminalKind.ACK)
        val thirdId = intentId("bounded-three")
        val thirdRow = row("bounded-three", 3, thirdId)
        val thirdIntent = intent(thirdId, thirdRow, active, DeliveryTerminalKind.ACK)
        assertTrue(
            journal(active, listOf(row, secondRow), listOf(valid, secondIntent))
                .prepareTerminalDrainAndFence(request(active, "within-bound", maxEffects = 2))
                is TerminalDrainAndFenceResult.Prepared,
        )
        assertCorrupt(
            journal(active, listOf(row, secondRow, thirdRow), listOf(valid, secondIntent, thirdIntent)),
            active,
            TerminalDrainAndFenceFailureReason.EFFECT_LIMIT_EXCEEDED,
            maxEffects = 2,
        )
    }

    private fun assertMismatchUnchanged(journal: DeliveryJournal, request: TerminalDrainAndFenceRequest) {
        assertEquals(
            TerminalDrainAndFenceResult.OwnershipMismatch(
                journal = journal,
                requested = request.attempt,
                actualOwner = journal.activeOwnerBareJid?.let(::DeliveryOwnerBareJid),
                actualAttempt = journal.activeOwnerBareJid?.let(journal.owners::get)?.activeAttempt,
            ),
            journal.prepareTerminalDrainAndFence(request),
        )
    }

    private fun assertCorrupt(
        journal: DeliveryJournal,
        attempt: DeliveryAttemptRef,
        reason: TerminalDrainAndFenceFailureReason,
        maxEffects: Int = 8,
    ) {
        assertEquals(
            TerminalDrainAndFenceResult.Corrupt(journal, reason),
            journal.prepareTerminalDrainAndFence(request(attempt, "receipt-$reason", maxEffects)),
        )
    }

    private fun journal(
        attempt: DeliveryAttemptRef,
        rows: List<QueuedOutboundMessage> = emptyList(),
        intents: List<DeliveryTerminalIntent> = emptyList(),
    ): DeliveryJournal = DeliveryJournal(
        activeOwnerBareJid = OWNER,
        owners = mapOf(
            OWNER to DeliveryOwnerJournal(
                activeAttempt = attempt,
                outboundRows = rows,
                terminalIntents = intents,
            ),
        ),
    )

    private fun request(
        attempt: DeliveryAttemptRef,
        receipt: String,
        maxEffects: Int = 8,
    ): TerminalDrainAndFenceRequest = TerminalDrainAndFenceRequest(
        attempt = attempt,
        receiptId = TerminalReceiptId(uuid(receipt)),
        originProcessEpoch = ProcessEpoch(uuid("epoch-$receipt")),
        nowMillis = 1_000,
        maxEffects = maxEffects,
    )

    private fun acknowledgedReceipt(attempt: DeliveryAttemptRef, id: String): TerminalReceipt = TerminalReceipt(
        owner = DeliveryOwnerBareJid(OWNER),
        attempt = attempt,
        id = TerminalReceiptId(uuid(id)),
        originProcessEpoch = ProcessEpoch(uuid("epoch-$id")),
        preparedAtMillis = 1,
        state = TerminalReceiptState.Acknowledged,
    )

    private fun DeliveryJournal.withRows(rows: List<QueuedOutboundMessage>): DeliveryJournal {
        val owner = checkNotNull(owners[OWNER])
        return copy(owners = owners + (OWNER to owner.copy(outboundRows = rows)))
    }

    private fun intent(
        id: DeliveryTerminalIntentId,
        row: QueuedOutboundMessage,
        attempt: DeliveryAttemptRef,
        kind: DeliveryTerminalKind,
    ): DeliveryTerminalIntent = DeliveryTerminalIntent(
        id = id,
        row = row.identity,
        expectedOwnership = OutboundOwnership.NativeOwned(attempt, social.waddle.android.client.prefs.NativeOutboundPhase.FRESH),
        kind = kind,
        createdAtMillis = 1,
    )

    private fun row(id: String, sequence: Long, intentId: DeliveryTerminalIntentId): QueuedOutboundMessage =
        QueuedOutboundDraft.create(
            ownerBareJid = OWNER,
            clientStanzaId = id,
            enqueuedAtMillis = 1,
            payload = QueuedOutboundPayload(
                target = QueuedOutboundTarget.Chat("peer@waddle.test"),
                content = QueuedOutboundContent("body-$id"),
            ),
        ).persisted(sequence, OutboundOwnership.Terminal(intentId))

    private fun duplicateIdRow(id: DeliveryTerminalIntentId): QueuedOutboundMessage = row("two", 2, id)

    private fun attempt(seed: String, owner: String = OWNER): DeliveryAttemptRef = DeliveryAttemptRef(
        ownerBareJid = owner,
        attemptId = DeliveryAttemptId(uuid("attempt-$seed-$owner")),
        nativeGeneration = NativeConnectionGeneration(1u),
    )

    private fun intentId(seed: String): DeliveryTerminalIntentId = DeliveryTerminalIntentId(uuid("intent-$seed"))

    private fun uuid(seed: String): String = UUID.nameUUIDFromBytes(seed.toByteArray()).toString()

    private companion object {
        const val OWNER = "icepuma@waddle.test"
        const val OTHER_OWNER = "other@waddle.test"
    }
}
