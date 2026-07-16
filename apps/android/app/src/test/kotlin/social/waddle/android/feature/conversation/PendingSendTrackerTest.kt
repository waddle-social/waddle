package social.waddle.android.feature.conversation

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.MessageSendExtras
import social.waddle.android.client.SendResult
import social.waddle.client.ffi.WaddleSendMessageOutcome

class PendingSendTrackerTest {
    private val tracker = PendingSendTracker()

    private fun row(localId: Long): PendingMessage =
        tracker.pending.value.single { it.localId == localId }

    @Test
    fun `sent outcome adopts the returned stanza id`() {
        val message = tracker.append("hello", extras = null, timestampMillis = 1_000L)

        val tracked = tracker.onSendResult(
            message.localId,
            SendResult(WaddleSendMessageOutcome.Sent("client-origin-id")),
        )

        assertTrue(tracked)
        val updated = row(message.localId)
        assertEquals("client-origin-id", updated.stanzaId)
        assertFalse(updated.queued)
        assertFalse(updated.acked)
        assertFalse(updated.failed)
    }

    @Test
    fun `queued outcome adopts the queue id and marks queued`() {
        val message = tracker.append("offline", extras = null, timestampMillis = 1_000L)

        val tracked = tracker.onSendResult(
            message.localId,
            SendResult(WaddleSendMessageOutcome.NotConnected, queuedId = "q-1"),
        )

        assertTrue(tracked)
        val updated = row(message.localId)
        assertEquals("q-1", updated.stanzaId)
        assertTrue(updated.queued)
        assertFalse(updated.failed)
    }

    @Test
    fun `permanent outcome without a queue id marks the row failed`() {
        val message = tracker.append("rejected", extras = null, timestampMillis = 1_000L)

        val tracked = tracker.onSendResult(
            message.localId,
            SendResult(WaddleSendMessageOutcome.StanzaError),
        )

        assertFalse(tracked)
        assertTrue(row(message.localId).failed)
    }

    @Test
    fun `ack beating the send continuation marks the row acked`() {
        val message = tracker.append("racy", extras = null, timestampMillis = 1_000L)

        tracker.onDeliveryAcked("s-1")
        tracker.onSendResult(message.localId, SendResult(WaddleSendMessageOutcome.Sent("s-1")))

        val updated = row(message.localId)
        assertTrue(updated.acked)
        assertFalse(updated.failed)
        assertFalse(updated.queued)
    }

    @Test
    fun `failure beating the send continuation wins over an ack`() {
        val message = tracker.append("racy", extras = null, timestampMillis = 1_000L)

        tracker.onDeliveryAcked("s-1")
        tracker.onDeliveryFailed("s-1")
        tracker.onSendResult(
            message.localId,
            SendResult(WaddleSendMessageOutcome.NotConnected, queuedId = "s-1"),
        )

        val updated = row(message.localId)
        assertTrue(updated.failed)
        assertFalse(updated.acked)
        assertFalse(updated.queued)
    }

    @Test
    fun `delivery ack marks the row without removing it`() {
        // 1:1 chats are never reflected back to the sending resource:
        // the acked optimistic row is the message's only representation
        // until the next MAM fetch and must not disappear.
        val message = tracker.append("hi there", extras = null, timestampMillis = 1_000L)
        tracker.onSendResult(message.localId, SendResult(WaddleSendMessageOutcome.Sent("dm-origin-id")))

        tracker.onDeliveryAcked("dm-origin-id")

        val updated = tracker.pending.value.single()
        assertTrue(updated.acked)
        assertEquals("hi there", updated.body)
    }

    @Test
    fun `queued row flips to failed when the queue drops it`() {
        val message = tracker.append("never makes it", extras = null, timestampMillis = 1_000L)
        tracker.onSendResult(
            message.localId,
            SendResult(WaddleSendMessageOutcome.NotConnected, queuedId = "q-2"),
        )

        // Cap eviction / permanent replay rejection surfaces as a
        // DeliveryFailed for the queue id.
        tracker.onDeliveryFailed("q-2")

        val updated = row(message.localId)
        assertTrue(updated.failed)
        assertFalse("failed row is retryable, no longer queued", updated.queued)
    }

    @Test
    fun `queued row acks after the replay without an echo`() {
        val message = tracker.append("dm while offline", extras = null, timestampMillis = 1_000L)
        tracker.onSendResult(
            message.localId,
            SendResult(WaddleSendMessageOutcome.NotConnected, queuedId = "q-3"),
        )

        // Reconnect: the drain sent it and the server 0198-acked the id.
        tracker.onDeliveryAcked("q-3")

        val updated = row(message.localId)
        assertTrue(updated.acked)
        assertFalse("delivered, no longer queued", updated.queued)
        assertFalse(updated.failed)
    }

    @Test
    fun `pruneAgainst removes only rows whose identity is stored`() {
        val echoed = tracker.append("echoed", extras = null, timestampMillis = 1_000L)
        tracker.onSendResult(echoed.localId, SendResult(WaddleSendMessageOutcome.Sent("s-echoed")))
        val inFlight = tracker.append("still flying", extras = null, timestampMillis = 1_000L)

        tracker.pruneAgainst(setOf("s-echoed", "unrelated"))

        assertEquals(listOf(inFlight.localId), tracker.pending.value.map { it.localId })
    }

    @Test
    fun `takeRetry removes and returns only failed rows`() {
        val extras = MessageSendExtras(threadId = "t1")
        val healthy = tracker.append("fine", extras = null, timestampMillis = 1_000L)
        val doomed = tracker.append("doomed", extras, timestampMillis = 1_000L)
        tracker.onSendResult(doomed.localId, SendResult(WaddleSendMessageOutcome.StanzaError))

        assertNull("non-failed rows are not retryable", tracker.takeRetry(healthy.localId))

        val taken = tracker.takeRetry(doomed.localId)
        assertEquals("doomed", taken?.body)
        assertEquals("retry keeps the reply/thread extras", extras, taken?.extras)
        assertEquals(listOf(healthy.localId), tracker.pending.value.map { it.localId })
    }
}
