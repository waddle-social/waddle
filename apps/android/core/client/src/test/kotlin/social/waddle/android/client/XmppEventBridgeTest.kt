package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import social.waddle.client.ffi.WaddleClientEvent

class XmppEventBridgeTest {
    private val bridge = XmppEventBridge()

    private fun received(): XmppEvent? = bridge.events.tryReceive().getOrNull()

    @Test
    fun `maps every client event variant`() {
        val message = testMessage()
        val presence = testPresence()
        val archived = testArchivedMessage()
        val call = testCallEvent()

        bridge.onEvent(WaddleClientEvent.Connected)
        assertEquals(XmppEvent.SessionReady, received())

        bridge.onEvent(WaddleClientEvent.Message(message))
        assertEquals(XmppEvent.Message(message), received())

        bridge.onEvent(WaddleClientEvent.Presence(presence))
        assertEquals(XmppEvent.Presence(presence), received())

        bridge.onEvent(WaddleClientEvent.MamResult(archived))
        assertEquals(XmppEvent.MamResult(archived), received())

        bridge.onEvent(WaddleClientEvent.DeliveryAcked("stanza-1"))
        assertEquals(XmppEvent.DeliveryAcked("stanza-1"), received())

        bridge.onEvent(WaddleClientEvent.DeliveryFailed("stanza-2"))
        assertEquals(XmppEvent.DeliveryFailed("stanza-2"), received())

        bridge.onEvent(WaddleClientEvent.Call(call))
        assertEquals(XmppEvent.Call(call), received())

        bridge.onEvent(WaddleClientEvent.Error("boom"))
        assertEquals(XmppEvent.Error("boom"), received())

        bridge.onEvent(WaddleClientEvent.Disconnected)
        assertEquals(XmppEvent.Disconnected, received())

        assertNull("channel should be drained", received())
    }
}
