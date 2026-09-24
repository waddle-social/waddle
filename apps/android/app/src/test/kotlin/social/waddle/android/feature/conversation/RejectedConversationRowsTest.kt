package social.waddle.android.feature.conversation

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.SendResult
import social.waddle.android.client.store.TimelineStore
import social.waddle.android.client.testMessage
import social.waddle.client.ffi.WaddleSendMessageOutcome

class RejectedConversationRowsTest {
    @Test
    fun `rejected stored echo stays failed through retry and screen recreation`() {
        val store = TimelineStore()
        store.setOwnBareJid("me@waddle.test")
        val peer = "missing@waddle.test"
        store.onLiveMessage(
            testMessage(
                id = "s-1", originId = "s-1", stanzaId = null,
                from = "me@waddle.test", to = peer, body = "rejected",
            ),
        )
        val tracker = PendingSendTracker()
        val pending = tracker.append("rejected", null, 1_000L)
        tracker.onSendResult(pending.localId, SendResult(WaddleSendMessageOutcome.Sent("s-1")))
        tracker.onDeliveryAcked("s-1")
        tracker.pruneAgainst(storedIdentityIdsOf(store.timeline(peer).value))

        store.rejectOutbound(peer, "s-1")
        tracker.onMessageRejected("s-1")
        tracker.onDeliveryAcked("s-1")
        val failed = visibleRows(store.timeline(peer).value, tracker.pending.value, null).single()
        assertTrue((failed as ConversationRow.Unconfirmed).message.failed)

        val retry = checkNotNull(tracker.takeRetry(pending.localId))
        tracker.append(retry.body, retry.extras, 2_000L)
        val rows = visibleRows(store.timeline(peer).value, tracker.pending.value, null)
        assertEquals(2, rows.size)
        assertTrue((rows.first() as ConversationRow.Stored).item.rejected)
        assertTrue(rows.last() is ConversationRow.Unconfirmed)

        val recreated = visibleRows(store.timeline(peer).value, PendingSendTracker().pending.value, null).single()
        assertTrue((recreated as ConversationRow.Stored).item.rejected)
    }
}
