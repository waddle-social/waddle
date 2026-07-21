package social.waddle.android.client

import androidx.datastore.preferences.core.mutablePreferencesOf
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.DeliveryAttemptId
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryJournal
import social.waddle.android.client.prefs.DeliveryJournalMutation
import social.waddle.android.client.prefs.DeliveryOwnerJournal
import social.waddle.android.client.prefs.DeliveryTerminalIntent
import social.waddle.android.client.prefs.DeliveryTerminalIntentId
import social.waddle.android.client.prefs.DeliveryTerminalKind
import social.waddle.android.client.prefs.NativeConnectionGeneration
import social.waddle.android.client.prefs.NativeOutboundPhase
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.ProcessEpoch
import social.waddle.android.client.prefs.QueuedOutboundContent
import social.waddle.android.client.prefs.QueuedOutboundDraft
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.QueuedOutboundPayload
import social.waddle.android.client.prefs.QueuedOutboundTarget
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.TerminalReceiptId
import social.waddle.android.client.prefs.TerminalReceiptState
import java.io.IOException
import java.util.UUID

class TerminalDrainAndFencePersistenceTest {
    @Test
    fun `prepared terminal fence is atomically persisted with its exact receipt and effects`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val attempt = attempt("active")
        val initial = journal(
            attempt = attempt,
            terminal = terminal("one"),
            terminalKind = DeliveryTerminalKind.NATIVE_FAILURE,
        )
        seed(prefs, initial)

        val result = prefs.persistTerminalDrainAndFence(request(attempt, "receipt"))
        val prepared = result as TerminalDrainAndFenceResult.Prepared
        val durable = prefs.deliveryJournal.first()

        assertEquals(prepared.journal, durable)
        assertEquals(prepared.receipt, durable.owners.getValue(OWNER).terminalReceipt)
        assertEquals(null, durable.owners.getValue(OWNER).activeAttempt)
        assertEquals(emptyList<DeliveryTerminalIntent>(), durable.owners.getValue(OWNER).terminalIntents)
        assertEquals(
            listOf(OutboundOwnership.Ready),
            durable.owners.getValue(OWNER).outboundRows.map(QueuedOutboundMessage::ownership),
        )
        val effects = (prepared.receipt.state as TerminalReceiptState.Pending).effects
        assertEquals(listOf(initial.owners.getValue(OWNER).outboundRows.single()), effects.map { it.row })
    }

    @Test
    fun `stable receipt retries are persisted idempotently and conflicts do not change storage`() = runTest {
        val prefs = SessionPrefs(FailingPreferencesDataStore())
        val attempt = attempt("active")
        seed(prefs, journal(attempt, terminal("one")))
        val request = request(attempt, "receipt")
        val prepared = prefs.persistTerminalDrainAndFence(request) as TerminalDrainAndFenceResult.Prepared

        assertEquals(
            TerminalDrainAndFenceResult.PriorReceiptPending(prepared.journal, prepared.receipt),
            prefs.persistTerminalDrainAndFence(request),
        )
        assertEquals(prepared.journal, prefs.deliveryJournal.first())
        assertConflictUnchanged(prefs, request(attempt, "different-receipt"), prepared.journal)
        assertConflictUnchanged(prefs, request(attempt("other"), "receipt"), prepared.journal)
    }

    @Test
    fun `zero effect acknowledged receipt is atomically persisted and idempotent`() = runTest {
        val prefs = SessionPrefs(FailingPreferencesDataStore())
        val attempt = attempt("active")
        seed(prefs, journal(attempt))
        val request = request(attempt, "receipt")

        val prepared = prefs.persistTerminalDrainAndFence(request) as TerminalDrainAndFenceResult.Prepared
        assertEquals(TerminalReceiptState.PreAcknowledged, prepared.receipt.state)
        assertEquals(
            TerminalDrainAndFenceResult.AlreadyAcknowledged(prepared.journal, prepared.receipt),
            prefs.persistTerminalDrainAndFence(request),
        )
        assertEquals(prepared.journal, prefs.deliveryJournal.first())
    }

    @Test
    fun `mismatch and corruption preserve the exact durable journal`() = runTest {
        val prefs = SessionPrefs(FailingPreferencesDataStore())
        val active = attempt("active")
        val corruptRow = terminal("one").copy(
            ownership = OutboundOwnership.NativeOwned(active, NativeOutboundPhase.FRESH),
        )
        val corrupt = journal(active, row = corruptRow, intents = emptyList())
        seed(prefs, corrupt)

        assertEquals(
            TerminalDrainAndFenceResult.OwnershipMismatch(
                journal = corrupt,
                requested = attempt("wrong"),
                actualOwner = social.waddle.android.client.prefs.DeliveryOwnerBareJid(OWNER),
                actualAttempt = active,
            ),
            prefs.persistTerminalDrainAndFence(request(attempt("wrong"), "receipt")),
        )
        assertEquals(corrupt, prefs.deliveryJournal.first())
        assertEquals(
            TerminalDrainAndFenceResult.Corrupt(
                corrupt,
                TerminalDrainAndFenceFailureReason.NATIVE_OWNED_ROW_REMAINS,
            ),
            prefs.persistTerminalDrainAndFence(request(active, "receipt")),
        )
        assertEquals(corrupt, prefs.deliveryJournal.first())
    }

    @Test
    fun `io failure before commit leaves no fence and retry prepares exactly once`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val attempt = attempt("active")
        val initial = journal(attempt, terminal("one"))
        seed(prefs, initial)
        store.failNextUpdate = true

        val failure = runCatching {
            prefs.persistTerminalDrainAndFence(request(attempt, "receipt"))
        }.exceptionOrNull()
        assertTrue(failure is IOException)
        assertEquals(initial, prefs.deliveryJournal.first())

        val prepared = prefs.persistTerminalDrainAndFence(request(attempt, "receipt"))
        assertTrue(prepared is TerminalDrainAndFenceResult.Prepared)
        assertEquals((prepared as TerminalDrainAndFenceResult.Prepared).journal, prefs.deliveryJournal.first())
    }

    @Test
    @OptIn(ExperimentalCoroutinesApi::class)
    fun `concurrent identical requests serialize behind the first pre-commit transaction`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val attempt = attempt("active")
        seed(prefs, journal(attempt, terminal("one")))
        val request = request(attempt, "receipt")
        val firstComputed = CompletableDeferred<Unit>()
        val releaseFirstCommit = CompletableDeferred<Unit>()
        store.installBeforeCommitReturnsOnce {
            firstComputed.complete(Unit)
            releaseFirstCommit.await()
        }

        val first = async { prefs.persistTerminalDrainAndFence(request) }
        firstComputed.await()
        val second = async { prefs.persistTerminalDrainAndFence(request) }
        runCurrent()
        assertFalse("second caller must be blocked behind the first transaction", second.isCompleted)
        releaseFirstCommit.complete(Unit)

        val outcomes = awaitAll(first, second)
        val prepared = outcomes.single { it is TerminalDrainAndFenceResult.Prepared } as TerminalDrainAndFenceResult.Prepared
        assertEquals(
            listOf(TerminalDrainAndFenceResult.PriorReceiptPending(prepared.journal, prepared.receipt)),
            outcomes.filterIsInstance<TerminalDrainAndFenceResult.PriorReceiptPending>(),
        )
        assertEquals(prepared.journal, prefs.deliveryJournal.first())
    }

    @Test
    fun `cancellation after commit preserves the receipt for an idempotent retry`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val attempt = attempt("active")
        seed(prefs, journal(attempt, terminal("one")))
        val enteredAfterCommit = CompletableDeferred<Unit>()
        store.installAfterCommitReturnsOnce {
            enteredAfterCommit.complete(Unit)
            awaitCancellation()
        }
        val caller = async { prefs.persistTerminalDrainAndFence(request(attempt, "receipt")) }

        enteredAfterCommit.await()
        val committed = prefs.deliveryJournal.first()
        caller.cancel()
        val cancellation = runCatching { caller.await() }.exceptionOrNull()
        assertTrue(cancellation is kotlinx.coroutines.CancellationException)
        val receipt = committed.owners.getValue(OWNER).terminalReceipt
        assertTrue(receipt != null)
        assertEquals(
            TerminalDrainAndFenceResult.PriorReceiptPending(committed, checkNotNull(receipt)),
            prefs.persistTerminalDrainAndFence(request(attempt, "receipt")),
        )
    }

    @Test
    fun `foreign owner state is preserved by the persisted transaction`() = runTest {
        val prefs = SessionPrefs(FailingPreferencesDataStore())
        val attempt = attempt("active")
        val foreignAttempt = attempt("foreign", FOREIGN_OWNER)
        val foreignReceipt = DeliveryJournal(
            activeOwnerBareJid = FOREIGN_OWNER,
            owners = mapOf(FOREIGN_OWNER to DeliveryOwnerJournal(activeAttempt = foreignAttempt)),
        ).prepareTerminalDrainAndFence(request(foreignAttempt, "foreign-receipt"))
            as TerminalDrainAndFenceResult.Prepared
        val foreign = foreignReceipt.journal.owners.getValue(FOREIGN_OWNER).copy(
            outboundRows = listOf(foreignReadyRow()),
        )
        val initial = journal(attempt).copy(owners = journal(attempt).owners + (FOREIGN_OWNER to foreign))
        seed(prefs, initial)

        val prepared = prefs.persistTerminalDrainAndFence(request(attempt, "receipt")) as TerminalDrainAndFenceResult.Prepared
        assertEquals(foreign, prepared.journal.owners[FOREIGN_OWNER])
        assertEquals(foreign, prefs.deliveryJournal.first().owners[FOREIGN_OWNER])
    }

    @Test
    fun `malformed persisted journal propagates instead of manufacturing a terminal fence`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val malformed = "{not-a-delivery-journal"
        store.updateData { mutablePreferencesOf(DELIVERY_JOURNAL_KEY to malformed) }

        val failure = runCatching {
            prefs.persistTerminalDrainAndFence(request(attempt("active"), "receipt"))
        }.exceptionOrNull()
        assertTrue(failure is social.waddle.android.client.prefs.DeliveryJournalDecodeException)
        assertEquals(malformed, store.data.first()[DELIVERY_JOURNAL_KEY])
    }

    @Test
    fun `mismatch and corruption preserve valid noncanonical raw journal bytes`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val active = attempt("active")
        val mismatchRaw = reorderedPrettyJournal(journal(active))
        writeRawJournal(store, mismatchRaw)

        assertTrue(
            prefs.persistTerminalDrainAndFence(request(attempt("wrong"), "receipt"))
                is TerminalDrainAndFenceResult.OwnershipMismatch,
        )
        assertEquals(mismatchRaw, store.data.first()[DELIVERY_JOURNAL_KEY])

        val corrupt = journal(
            attempt = active,
            row = terminal("corrupt").copy(
                ownership = OutboundOwnership.NativeOwned(active, NativeOutboundPhase.FRESH),
            ),
            intents = emptyList(),
        )
        val corruptRaw = reorderedPrettyJournal(corrupt)
        writeRawJournal(store, corruptRaw)
        assertEquals(
            TerminalDrainAndFenceFailureReason.NATIVE_OWNED_ROW_REMAINS,
            (
                prefs.persistTerminalDrainAndFence(request(active, "receipt"))
                as TerminalDrainAndFenceResult.Corrupt
            ).reason,
        )
        assertEquals(corruptRaw, store.data.first()[DELIVERY_JOURNAL_KEY])
    }

    @Test
    fun `unknown field in foreign owner journal fails strictly without rewriting raw state`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val active = attempt("active")
        val foreignAttempt = attempt("foreign", FOREIGN_OWNER)
        val journal = journal(active).copy(
            owners = journal(active).owners + (FOREIGN_OWNER to DeliveryOwnerJournal(activeAttempt = foreignAttempt)),
        )
        val raw = strictForeignUnknownRaw(journal)
        writeRawJournal(store, raw)

        val failure = runCatching {
            prefs.persistTerminalDrainAndFence(request(active, "receipt"))
        }.exceptionOrNull()
        assertTrue(failure is social.waddle.android.client.prefs.DeliveryJournalDecodeException)
        assertEquals(raw, store.data.first()[DELIVERY_JOURNAL_KEY])
    }

    private suspend fun assertConflictUnchanged(
        prefs: SessionPrefs,
        request: TerminalDrainAndFenceRequest,
        expectedJournal: DeliveryJournal,
    ) {
        assertEquals(
            TerminalDrainAndFenceResult.Corrupt(
                expectedJournal,
                TerminalDrainAndFenceFailureReason.RECEIPT_CONFLICT,
            ),
            prefs.persistTerminalDrainAndFence(request),
        )
        assertEquals(expectedJournal, prefs.deliveryJournal.first())
    }

    private suspend fun seed(prefs: SessionPrefs, journal: DeliveryJournal) {
        prefs.updateDeliveryJournal { DeliveryJournalMutation(journal, Unit) }
    }

    private suspend fun writeRawJournal(store: FailingPreferencesDataStore, raw: String) {
        store.updateData { mutablePreferencesOf(DELIVERY_JOURNAL_KEY to raw) }
    }

    private fun journal(
        attempt: DeliveryAttemptRef,
        terminal: QueuedOutboundMessage? = null,
        terminalKind: DeliveryTerminalKind = DeliveryTerminalKind.ACK,
        row: QueuedOutboundMessage? = terminal,
        intents: List<DeliveryTerminalIntent> = terminal?.let { listOf(intent(it, attempt, terminalKind)) } ?: emptyList(),
    ): DeliveryJournal = DeliveryJournal(
        activeOwnerBareJid = OWNER,
        owners = mapOf(
            OWNER to DeliveryOwnerJournal(
                activeAttempt = attempt,
                outboundRows = listOfNotNull(row),
                terminalIntents = intents,
            ),
        ),
    )

    private fun terminal(id: String): QueuedOutboundMessage {
        val intentId = DeliveryTerminalIntentId(uuid("intent-$id"))
        return QueuedOutboundDraft.create(
            ownerBareJid = OWNER,
            clientStanzaId = id,
            enqueuedAtMillis = 1,
            payload = QueuedOutboundPayload(
                target = QueuedOutboundTarget.Chat("peer@waddle.test"),
                content = QueuedOutboundContent("body-$id"),
            ),
        ).persisted(sequence = 1, ownership = OutboundOwnership.Terminal(intentId))
    }

    private fun intent(
        row: QueuedOutboundMessage,
        attempt: DeliveryAttemptRef,
        kind: DeliveryTerminalKind,
    ): DeliveryTerminalIntent = DeliveryTerminalIntent(
        id = (row.ownership as OutboundOwnership.Terminal).intentId,
        row = row.identity,
        expectedOwnership = OutboundOwnership.NativeOwned(attempt, NativeOutboundPhase.FRESH),
        kind = kind,
        createdAtMillis = 1,
    )

    private fun request(attempt: DeliveryAttemptRef, receipt: String): TerminalDrainAndFenceRequest =
        TerminalDrainAndFenceRequest(
            attempt = attempt,
            receiptId = TerminalReceiptId(uuid("receipt-$receipt")),
            originProcessEpoch = ProcessEpoch(uuid("epoch-$receipt")),
            nowMillis = 1,
            maxEffects = 8,
        )

    private fun attempt(seed: String, owner: String = OWNER): DeliveryAttemptRef = DeliveryAttemptRef(
        ownerBareJid = owner,
        attemptId = DeliveryAttemptId(uuid("attempt-$seed-$owner")),
        nativeGeneration = NativeConnectionGeneration(1u),
    )

    private fun uuid(seed: String): String = UUID.nameUUIDFromBytes(seed.toByteArray()).toString()

    private fun reorderedPrettyJournal(journal: DeliveryJournal): String {
        val root = journalJson.encodeToJsonElement(DeliveryJournal.serializer(), journal).jsonObject
        val reordered = buildJsonObject {
            put("owners", root.getValue("owners"))
            put("activeOwnerBareJid", root.getValue("activeOwnerBareJid"))
            put("schemaVersion", root.getValue("schemaVersion"))
        }
        return prettyJournalJson.encodeToString(JsonObject.serializer(), reordered)
    }

    private fun strictForeignUnknownRaw(journal: DeliveryJournal): String = journalJson
        .encodeToString(journal)
        .replaceFirst(
            "\"$FOREIGN_OWNER\":{",
            "\"$FOREIGN_OWNER\":{\"unknownFutureField\":true,",
        )

    private fun foreignReadyRow(): QueuedOutboundMessage = QueuedOutboundDraft.create(
        ownerBareJid = FOREIGN_OWNER,
        clientStanzaId = "foreign-ready",
        enqueuedAtMillis = 1,
        payload = QueuedOutboundPayload(
            target = QueuedOutboundTarget.Chat("peer@waddle.test"),
            content = QueuedOutboundContent("foreign"),
        ),
    ).persisted(sequence = 1, ownership = OutboundOwnership.Ready)

    private companion object {
        const val OWNER = "icepuma@waddle.test"
        const val FOREIGN_OWNER = "foreign@waddle.test"
        val DELIVERY_JOURNAL_KEY = stringPreferencesKey("delivery_journal_v1")
        val journalJson = Json { encodeDefaults = true }
        val prettyJournalJson = Json {
            encodeDefaults = true
        prettyPrint = true
        }
    }
}
