package social.waddle.android.feature.conversation

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.DeliveryOutcomeRef
import social.waddle.android.client.MessageSendExtras
import social.waddle.android.client.SendResult
import social.waddle.android.client.prefs.DeliveryIncarnation
import social.waddle.android.client.prefs.DeliveryPayloadDigest
import social.waddle.android.client.prefs.DeliveryRowIdentity
import social.waddle.android.client.prefs.DeliverySource
import social.waddle.client.ffi.WaddleSendMessageOutcome
import java.util.UUID

class PendingSendTrackerTest {
    private val tracker = PendingSendTracker()

    private fun row(localId: Long): PendingMessage =
        tracker.pending.value.single { it.localId == localId }

    @Test
    fun `sent outcome adopts the returned stanza id`() {
        val message = tracker.append("hello", extras = null, timestampMillis = 1_000L)

        val tracked = tracker.onSendResult(
            message.localId,
            SendResult(
                WaddleSendMessageOutcome.Sent("client-origin-id"),
                delivery("client-origin-id"),
            ),
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
            SendResult(
                WaddleSendMessageOutcome.NotConnected,
                delivery("q-1"),
            ),
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
        val delivery = delivery("s-1")

        tracker.onDeliveryAcked(delivery)
        tracker.onSendResult(
            message.localId,
            SendResult(WaddleSendMessageOutcome.Sent("s-1"), delivery),
        )

        val updated = row(message.localId)
        assertTrue(updated.acked)
        assertFalse(updated.failed)
        assertFalse(updated.queued)
    }

    @Test
    fun `failure beating the send continuation wins over an ack`() {
        val message = tracker.append("racy", extras = null, timestampMillis = 1_000L)
        val delivery = delivery("s-1")

        tracker.onDeliveryAcked(delivery)
        tracker.onDeliveryFailed(delivery)
        tracker.onSendResult(
            message.localId,
            SendResult(WaddleSendMessageOutcome.NotConnected, delivery),
        )

        val updated = row(message.localId)
        assertTrue(updated.failed)
        assertFalse(updated.acked)
        assertFalse(updated.queued)
    }

    @Test
    fun `ack after automatic retry clears the exact rows transport failure`() {
        val message = tracker.append("retry me", extras = null, timestampMillis = 1_000L)
        val delivery = delivery("s-retry")
        tracker.onSendResult(
            message.localId,
            SendResult(WaddleSendMessageOutcome.Sent("s-retry"), delivery),
        )

        tracker.onDeliveryFailed(delivery)
        assertTrue(row(message.localId).failed)
        tracker.onDeliveryAcked(delivery)

        assertTrue(row(message.localId).acked)
        assertFalse(row(message.localId).failed)
    }

    @Test
    fun `delivery ack marks the row without removing it`() {
        // 1:1 chats are never reflected back to the sending resource:
        // the acked optimistic row is the message's only representation
        // until the next MAM fetch and must not disappear.
        val message = tracker.append("hi there", extras = null, timestampMillis = 1_000L)
        val delivery = delivery("dm-origin-id")
        tracker.onSendResult(
            message.localId,
            SendResult(WaddleSendMessageOutcome.Sent("dm-origin-id"), delivery),
        )

        tracker.onDeliveryAcked(delivery)

        val updated = tracker.pending.value.single()
        assertTrue(updated.acked)
        assertEquals("hi there", updated.body)
    }

    @Test
    fun `queued row flips to failed when the queue drops it`() {
        val message = tracker.append("never makes it", extras = null, timestampMillis = 1_000L)
        tracker.onSendResult(
            message.localId,
            SendResult(WaddleSendMessageOutcome.NotConnected, delivery("q-2")),
        )

        // Cap eviction / permanent replay rejection surfaces as a
        // DeliveryFailed for the queue id.
        tracker.onDeliveryFailed(delivery("q-2"))

        val updated = row(message.localId)
        assertTrue(updated.failed)
        assertFalse("failed row is retryable, no longer queued", updated.queued)
    }

    @Test
    fun `queued row acks after the replay without an echo`() {
        val message = tracker.append("dm while offline", extras = null, timestampMillis = 1_000L)
        tracker.onSendResult(
            message.localId,
            SendResult(WaddleSendMessageOutcome.NotConnected, delivery("q-3")),
        )

        // Reconnect: the drain sent it and the server 0198-acked the id.
        tracker.onDeliveryAcked(delivery("q-3"))

        val updated = row(message.localId)
        assertTrue(updated.acked)
        assertFalse("delivered, no longer queued", updated.queued)
        assertFalse(updated.failed)
    }

    @Test
    fun `same stanza id from another owner cannot settle the active row`() {
        val message = tracker.append("hello", extras = null, timestampMillis = 1_000L)
        val active = delivery("same-id")
        tracker.onSendResult(
            message.localId,
            SendResult(WaddleSendMessageOutcome.Sent("same-id"), active),
        )

        tracker.onDeliveryAcked(
            active.copy(
                identity = active.identity.copy(ownerBareJid = "bob@waddle.test"),
            ),
        )

        assertFalse(row(message.localId).acked)
        assertFalse(row(message.localId).failed)
    }

    @Test
    fun `pruneAgainst removes only rows whose identity is stored`() {
        val echoed = tracker.append("echoed", extras = null, timestampMillis = 1_000L)
        tracker.onSendResult(
            echoed.localId,
            SendResult(WaddleSendMessageOutcome.Sent("s-echoed"), delivery("s-echoed")),
        )
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

    @Test
    fun `pruning a stored row settles its delivery-id bookkeeping`() {
        // An ack recorded before pruning must not linger: a NEW send that
        // happens to reuse the id (queue replay across sessions) would
        // otherwise adopt a stale acked flag.
        val message = tracker.append("hello", extras = null, timestampMillis = 1_000L)
        val original = delivery("id-1", "original")
        tracker.onSendResult(
            message.localId,
            SendResult(WaddleSendMessageOutcome.Sent("id-1"), original),
        )
        tracker.onDeliveryAcked(original)

        tracker.pruneAgainst(setOf("id-1"))
        assertTrue(tracker.pending.value.isEmpty())

        val fresh = tracker.append("again", extras = null, timestampMillis = 2_000L)
        tracker.onSendResult(
            fresh.localId,
            SendResult(
                WaddleSendMessageOutcome.Sent("id-1"),
                delivery("id-1", "fresh"),
            ),
        )
        assertFalse(row(fresh.localId).acked)
    }

    @Test
    fun `delivery identity sets stay bounded for callbacks that never match a row`() {
        repeat(300) { tracker.onDeliveryAcked(delivery("orphan-$it")) }
        // Oldest orphans evicted: a late send adopting an evicted id is
        // simply unacked (harmless), while recent ids still race-resolve.
        val message = tracker.append("late", extras = null, timestampMillis = 1_000L)
        tracker.onSendResult(
            message.localId,
            SendResult(
                WaddleSendMessageOutcome.Sent("orphan-0"),
                delivery("orphan-0"),
            ),
        )
        assertFalse(row(message.localId).acked)

        val recent = tracker.append("recent", extras = null, timestampMillis = 2_000L)
        tracker.onSendResult(
            recent.localId,
            SendResult(
                WaddleSendMessageOutcome.Sent("orphan-299"),
                delivery("orphan-299"),
            ),
        )
        assertTrue(row(recent.localId).acked)
    }

    private fun delivery(
        stanzaId: String,
        incarnationSeed: String = stanzaId,
    ): DeliveryOutcomeRef = DeliveryOutcomeRef(
        identity = DeliveryRowIdentity(
            ownerBareJid = "icepuma@waddle.test",
            clientStanzaId = stanzaId,
            incarnation = DeliveryIncarnation(
                UUID.nameUUIDFromBytes(incarnationSeed.toByteArray()).toString(),
            ),
            payloadDigest = DeliveryPayloadDigest("v1:sha256:${"0".repeat(64)}"),
        ),
        source = DeliverySource.Composer,
    )
}
