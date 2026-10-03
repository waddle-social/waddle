package social.waddle.android.client.store

import org.junit.Assert.assertEquals
import org.junit.Test
import social.waddle.android.client.testMessage

class DmStoreTest {
    private val store = DmStore()

    @Test
    fun `seed publishes newest-first from an oldest-to-newest list`() {
        store.seed(listOf("alice@waddle.test", "bob@waddle.test"))

        assertEquals(listOf("bob@waddle.test", "alice@waddle.test"), store.peers.value)
    }

    @Test
    fun `live message moves an existing peer to the front`() {
        store.seed(listOf("alice@waddle.test", "bob@waddle.test"))

        store.onChatMessage(
            ownBareJid = "me@waddle.test",
            message = testMessage(from = "alice@waddle.test/phone", to = "me@waddle.test"),
        )

        assertEquals(listOf("alice@waddle.test", "bob@waddle.test"), store.peers.value)
    }

    @Test
    fun `own sent carbon attributes the conversation to the recipient`() {
        store.onChatMessage(
            ownBareJid = "me@waddle.test",
            message = testMessage(from = "me@waddle.test/laptop", to = "carol@waddle.test"),
        )

        assertEquals(listOf("carol@waddle.test"), store.peers.value)
    }

    @Test
    fun `muc and non-chat messages never register a peer`() {
        store.onChatMessage(
            ownBareJid = "me@waddle.test",
            message = testMessage(
                from = "room@muc.waddle.test/alice",
                messageType = "groupchat",
                isMuc = true,
            ),
        )
        store.onChatMessage(
            ownBareJid = "me@waddle.test",
            message = testMessage(from = "alice@waddle.test", messageType = "headline"),
        )

        assertEquals(emptyList<String>(), store.peers.value)
    }

    @Test
    fun `room private messages and invites never register the room as a peer`() {
        // XEP-0045 PM received: type=chat from the occupant JID + muc#user marker.
        store.onChatMessage(
            ownBareJid = "me@waddle.test",
            message = testMessage(from = "room@muc.waddle.test/alice", mucUser = true),
        )
        // Its sent-carbon twin: from the account, addressed to the occupant JID.
        store.onChatMessage(
            ownBareJid = "me@waddle.test",
            message = testMessage(
                from = "me@waddle.test/laptop",
                to = "room@muc.waddle.test/alice",
                mucUser = true,
            ),
        )
        // Mediated invite: type=normal from the bare room.
        store.onChatMessage(
            ownBareJid = "me@waddle.test",
            message = testMessage(
                from = "room@muc.waddle.test",
                messageType = "normal",
                mucUser = true,
            ),
        )

        assertEquals(emptyList<String>(), store.peers.value)
    }

    @Test
    fun `self-messages and clear leave no peers behind`() {
        store.onChatMessage(
            ownBareJid = "me@waddle.test",
            message = testMessage(from = "me@waddle.test/phone", to = "me@waddle.test/laptop"),
        )
        assertEquals(emptyList<String>(), store.peers.value)

        store.seed(listOf("alice@waddle.test"))
        store.clear()
        assertEquals(emptyList<String>(), store.peers.value)
    }
}
