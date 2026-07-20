package social.waddle.android.client

import java.util.UUID
import kotlinx.coroutines.CoroutineScope
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import social.waddle.android.client.DeliveryJournalStore.EnqueueResult
import social.waddle.android.client.DeliveryJournalStore.LiveAdmissionResult
import social.waddle.android.client.prefs.DeliveryAttemptId
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryCallbackRef
import social.waddle.android.client.prefs.DeliveryJournalMutation
import social.waddle.android.client.prefs.DeliveryOwnerBareJid
import social.waddle.android.client.prefs.DeliverySource
import social.waddle.android.client.prefs.NativeConnectionGeneration
import social.waddle.android.client.prefs.NativeOutboundPhase
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.QueuedOutboundContent
import social.waddle.android.client.prefs.QueuedOutboundDraft
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.QueuedOutboundPayload
import social.waddle.android.client.prefs.QueuedOutboundTarget
import social.waddle.android.client.prefs.ProcessEpoch
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.TerminalReceipt
import social.waddle.android.client.prefs.TerminalReceiptEffect
import social.waddle.android.client.prefs.TerminalReceiptId
import social.waddle.android.client.prefs.TerminalReceiptClaimState
import social.waddle.android.client.prefs.TerminalReceiptState

internal fun terminalWorkerDraft(id: String): QueuedOutboundDraft = QueuedOutboundDraft.create(
    ownerBareJid = TERMINAL_WORKER_OWNER,
    clientStanzaId = id,
    enqueuedAtMillis = 1_000,
    payload = QueuedOutboundPayload(
        target = QueuedOutboundTarget.Chat("peer@waddle.test"),
        content = QueuedOutboundContent("body-$id"),
    ),
    source = DeliverySource.Composer,
)

internal fun terminalWorkerStored(result: EnqueueResult): QueuedOutboundMessage =
    (result as EnqueueResult.Stored).row

internal fun terminalWorkerClaimed(result: LiveAdmissionResult): QueuedOutboundMessage =
    (result as LiveAdmissionResult.Claimed).row

internal suspend fun seedNativeOwnedTerminalRows(
    prefs: SessionPrefs,
    queue: DeliveryJournalStore,
    attempt: DeliveryAttemptRef,
    count: Int,
): List<QueuedOutboundMessage> {
    val readyRows = (1..count).map { index ->
        terminalWorkerStored(queue.enqueueReady(terminalWorkerDraft("m-$index")))
    }
    return prefs.updateDeliveryJournal { journal ->
        val owner = checkNotNull(journal.owners[TERMINAL_WORKER_OWNER])
        val nativeRows = owner.outboundRows.map { row ->
            row.copy(
                ownership = OutboundOwnership.NativeOwned(
                    attempt,
                    NativeOutboundPhase.FRESH,
                ),
            )
        }
        DeliveryJournalMutation(
            journal = journal.copy(
                owners = journal.owners + (
                    TERMINAL_WORKER_OWNER to owner.copy(outboundRows = nativeRows)
                ),
            ),
            result = nativeRows,
        )
    }.also { nativeRows ->
        check(nativeRows.map { it.identity } == readyRows.map { it.identity })
    }
}

internal const val TERMINAL_WORKER_OWNER = "alice@waddle.test"
internal const val TERMINAL_WORKER_SIGNAL_COUNT = 258

internal fun terminalWorkerUuid(seed: String): String = UUID.nameUUIDFromBytes(seed.toByteArray()).toString()

internal fun pendingTerminalReceipt(
    owner: String,
    seed: String,
    effectCount: Int = 1,
): TerminalReceipt {
    val attempt = DeliveryAttemptRef(
        ownerBareJid = owner,
        attemptId = DeliveryAttemptId(terminalWorkerUuid("$seed-attempt")),
        nativeGeneration = social.waddle.android.client.prefs.NativeConnectionGeneration(1u),
    )
    val rows = (0 until effectCount).map { index ->
        QueuedOutboundDraft.create(
            ownerBareJid = owner,
            clientStanzaId = "$seed-row-$index",
            enqueuedAtMillis = (index + 1).toLong(),
            payload = QueuedOutboundPayload(
                target = QueuedOutboundTarget.Chat("peer@waddle.test"),
                content = QueuedOutboundContent("$seed-$index"),
            ),
        ).persisted(index.toLong() + 1, OutboundOwnership.Ready)
    }
    return TerminalReceipt(
        owner = DeliveryOwnerBareJid(owner),
        attempt = attempt,
        id = TerminalReceiptId(terminalWorkerUuid("$seed-receipt")),
        originProcessEpoch = ProcessEpoch(terminalWorkerUuid("$seed-origin")),
        preparedAtMillis = 1,
        state = TerminalReceiptState.Pending(
            TerminalReceiptClaimState.Unclaimed,
            rows.map { row ->
                TerminalReceiptEffect.Acknowledged(DeliveryCallbackRef(row.identity, attempt), row)
            },
        ),
    )
}

internal fun terminalRun(
    worker: DeliveryTerminalWorker,
    scope: CoroutineScope,
    onExit: suspend (WorkerExit) -> Unit = {},
): DeliveryTerminalWorker.Run = worker.start(
    scope,
    WorkerOwnership(
        SessionLifecycleRef.create(TERMINAL_WORKER_OWNER),
        WorkerKind.DELIVERY_TERMINAL,
        WorkerGeneration.random(),
    ),
    {},
    onExit,
)

internal suspend fun assertRequested(run: DeliveryTerminalWorker.Run) {
    run.requestStop()
    val outcome = run.awaitExit(1_000)
    assertTrue(outcome is WorkerAwaitOutcome.Exited)
    assertEquals(WorkerExitReason.RequestedStop, (outcome as WorkerAwaitOutcome.Exited).exit.reason)
}
