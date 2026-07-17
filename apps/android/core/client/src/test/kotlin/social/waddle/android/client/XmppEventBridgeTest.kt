package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import social.waddle.android.client.prefs.DeliveryAttemptId
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.NativeConnectionGeneration
import social.waddle.android.client.prefs.toFfi
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleDeliveryAttemptTransition
import social.waddle.client.ffi.WaddleDeliveryStanzaId
import social.waddle.client.ffi.WaddleNativeDeliverySignal
import social.waddle.client.ffi.WaddleSessionReadyKind

class XmppEventBridgeTest {
    private val owner = "icepuma@waddle.test"
    private val attempt = DeliveryAttemptRef(
        ownerBareJid = owner,
        attemptId = DeliveryAttemptId("00000000-0000-4000-8000-000000000001"),
        nativeGeneration = NativeConnectionGeneration(7u),
    )

    @Test
    fun `maps every non-control client event variant without guessing identity`() {
        val message = testMessage()
        val presence = testPresence()
        val archived = testArchivedMessage()
        val call = testCallEvent()

        assertEquals(
            XmppEvent.SessionReady(SessionReadyKind.FRESH, attempt),
            WaddleClientEvent.SessionReady(
                WaddleSessionReadyKind.FRESH,
                attempt.toFfi(),
            ).toXmppEvent(owner),
        )
        assertEquals(
            XmppEvent.Message(message),
            WaddleClientEvent.Message(message).toXmppEvent(owner),
        )
        assertEquals(
            XmppEvent.Presence(presence),
            WaddleClientEvent.Presence(presence).toXmppEvent(owner),
        )
        assertEquals(
            XmppEvent.MamResult(archived),
            WaddleClientEvent.MamResult(archived).toXmppEvent(owner),
        )
        assertEquals(
            XmppEvent.NativeDeliveryAcked(attempt, "stanza-1"),
            WaddleClientEvent.DeliveryAcked(signal("stanza-1")).toXmppEvent(owner),
        )
        assertEquals(
            XmppEvent.NativeDeliveryFailed(attempt, "stanza-2"),
            WaddleClientEvent.DeliveryFailed(signal("stanza-2")).toXmppEvent(owner),
        )
        assertEquals(
            XmppEvent.Call(call),
            WaddleClientEvent.Call(call).toXmppEvent(owner),
        )
        assertEquals(
            XmppEvent.Error("boom"),
            WaddleClientEvent.Error("boom").toXmppEvent(owner),
        )
        assertEquals(
            XmppEvent.Disconnected,
            WaddleClientEvent.Disconnected.toXmppEvent(owner),
        )
    }

    @Test
    fun `resume controls never escape into the domain event stream`() {
        assertNull(
            WaddleClientEvent.ResumeStateChanged(
                attempt = attempt.toFfi(),
                state = testResumeState(),
            ).toXmppEvent(owner),
        )
        assertNull(
            WaddleClientEvent.ResumeFailed(
                transition = WaddleDeliveryAttemptTransition(
                    old = attempt.toFfi(),
                    fresh = attempt.copy(
                        attemptId = DeliveryAttemptId(
                            "00000000-0000-4000-8000-000000000002",
                        ),
                        nativeGeneration = NativeConnectionGeneration(8u),
                    ).toFfi(),
                ),
                affected = listOf(WaddleDeliveryStanzaId("stanza-1")),
            ).toXmppEvent(owner),
        )
    }

    private fun signal(stanzaId: String): WaddleNativeDeliverySignal =
        WaddleNativeDeliverySignal(
            attempt = attempt.toFfi(),
            stanzaId = WaddleDeliveryStanzaId(stanzaId),
        )
}
