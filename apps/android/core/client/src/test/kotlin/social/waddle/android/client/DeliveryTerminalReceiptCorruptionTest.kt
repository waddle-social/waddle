package social.waddle.android.client

import androidx.datastore.preferences.core.mutablePreferencesOf
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.SessionPrefs

@OptIn(ExperimentalCoroutinesApi::class)
class DeliveryTerminalReceiptCorruptionTest {
    @Test
    fun `persisted journal decode failure fences before callback and preserves raw value`() = runTest {
        val store = InMemoryPreferencesDataStore()
        val key = stringPreferencesKey("delivery_journal_v1")
        store.updateData { mutablePreferencesOf(key to "{not-json") }
        val events = mutableListOf<XmppEvent>()
        val run = DeliveryTerminalWorker(
            DeliveryJournalStore(SessionPrefs(store)),
            events::add,
            evidence = WorkerExitExceptionEvidence(),
        ).start(
            this,
            WorkerOwnership(
                SessionLifecycleRef.create(OWNER),
                WorkerKind.DELIVERY_TERMINAL,
                WorkerGeneration.random(),
            ),
            {},
            {},
        )

        runCurrent()

        val exit = (run.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
        val kind = (exit.reason as WorkerExitReason.UnexpectedFailure).kind
        val failure = (kind as WorkerFailureKind.TERMINAL_RECEIPT_APPLICATION).failure
        assertEquals(
            TerminalReceiptApplicationFailure.DiscoveryCorrupt(
                social.waddle.android.client.prefs.DeliveryOwnerBareJid(OWNER),
                TerminalReceiptCorruption.PERSISTED_DECODE_FAILURE,
            ),
            failure,
        )
        assertTrue(events.isEmpty())
        assertEquals("{not-json", store.data.first()[key])
    }

    private companion object {
        const val OWNER = "alice@waddle.test"
    }
}
