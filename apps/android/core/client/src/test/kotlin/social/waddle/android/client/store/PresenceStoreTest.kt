package social.waddle.android.client.store

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.HAT_URI_BOT
import social.waddle.android.client.testPresence
import social.waddle.client.ffi.WaddleMucAffiliation
import social.waddle.client.ffi.WaddleMucRole
import social.waddle.client.ffi.WaddlePresenceHat

class PresenceStoreTest {
    private val store = PresenceStore().apply { setOwnBareJid("Me@Waddle.Test/phone") }

    private fun contact(from: String, type: String = "available", show: String? = null, idle: String? = null) =
        testPresence(from = from, presenceType = type, show = show, idleSince = idle)

    private fun occupant(room: String, nick: String, real: String? = null, type: String = "available") =
        testPresence(
            from = "$room/$nick",
            presenceType = type,
            mucRole = if (type == "unavailable") WaddleMucRole.NONE else WaddleMucRole.PARTICIPANT,
            mucAffiliation = WaddleMucAffiliation.MEMBER,
            mucJid = real,
        )

    @Test
    fun `non-availability presence types are never stored`() {
        listOf("subscribe", "subscribed", "unsubscribe", "unsubscribed", "probe", "error").forEach { type ->
            store.onPresence(contact("alice@waddle.test/phone", type = type))
            store.onPresence(testPresence(from = "room@muc.waddle.test/bob", presenceType = type))
        }
        assertTrue(store.contacts.value.isEmpty())
        assertTrue(store.occupants.value.isEmpty())
    }

    @Test
    fun `an error presence does not unseat an online contact`() {
        store.onPresence(contact("alice@waddle.test/phone"))
        store.onPresence(contact("alice@waddle.test/phone", type = "error"))
        assertEquals(PresenceShow.AVAILABLE, store.contacts.value["alice@waddle.test"]?.show)
    }

    @Test
    fun `resources collapse in ADR-010 order`() {
        fun showWith(vararg shows: String?): PresenceShow? {
            val local = PresenceStore()
            shows.forEachIndexed { index, show -> local.onPresence(contact("bob@waddle.test/r$index", show = show)) }
            return local.contacts.value["bob@waddle.test"]?.show
        }
        assertEquals(PresenceShow.DND, showWith(null, "dnd", "away"))
        assertEquals(PresenceShow.AVAILABLE, showWith("xa", "away", null))
        assertEquals(PresenceShow.AVAILABLE, showWith("away", "chat"))
        assertEquals(PresenceShow.AWAY, showWith("xa", "away"))
        assertEquals(PresenceShow.XA, showWith("xa"))
    }

    @Test
    fun `the rendered presence is the newest resource with the winning show`() {
        store.onPresence(contact("bob@waddle.test/a", show = "away", idle = "2026-10-16T09:00:00Z"))
        store.onPresence(contact("bob@waddle.test/b", show = "xa", idle = "2026-10-16T08:00:00Z"))
        store.onPresence(contact("bob@waddle.test/c", show = "away", idle = "2026-10-16T10:00:00Z"))
        val collapsed = store.contacts.value["bob@waddle.test"]
        assertEquals(PresenceShow.AWAY, collapsed?.show)
        assertEquals("2026-10-16T10:00:00Z", collapsed?.presence?.idleSince)
    }

    @Test
    fun `unavailable resources drop out and the last one makes the contact offline`() {
        store.onPresence(contact("bob@waddle.test/a", show = "dnd"))
        store.onPresence(contact("bob@waddle.test/b"))
        store.onPresence(contact("bob@waddle.test/a", type = "unavailable"))
        assertEquals(PresenceShow.AVAILABLE, store.contacts.value["bob@waddle.test"]?.show)
        store.onPresence(contact("bob@waddle.test/b", type = "unavailable"))
        assertNull(store.contacts.value["bob@waddle.test"])
    }

    @Test
    fun `a bare unavailable covers every resource`() {
        store.onPresence(contact("bob@waddle.test/a"))
        store.onPresence(contact("bob@waddle.test/b", show = "away"))
        store.onPresence(contact("bob@waddle.test", type = "unavailable"))
        assertTrue(store.contacts.value.isEmpty())
    }

    @Test
    fun `contacts are keyed by normalised bare jid and resolve any case`() {
        store.onPresence(contact("Alice@Waddle.Test/Phone", show = "away"))
        store.onPresence(contact("alice@waddle.test/laptop"))
        assertEquals(setOf("alice@waddle.test"), store.contacts.value.keys)
        assertEquals(PresenceShow.AVAILABLE, store.contacts.value["ALICE@waddle.test"]?.show)
        assertEquals("alice@waddle.test", store.contacts.value["alice@WADDLE.test/x"]?.bareJid)
    }

    @Test
    fun `the account's own resources are never a contact`() {
        store.onPresence(contact("me@waddle.test/laptop"))
        store.onPresence(contact("ME@waddle.test/tablet", show = "dnd"))
        assertTrue(store.contacts.value.isEmpty())
    }

    @Test
    fun `occupant rooms are normalised like the nick and muji stores`() {
        store.onPresence(occupant("Room@Muc.Waddle.Test", "Alice", real = "alice@waddle.test/phone"))
        store.onPresence(occupant("room@muc.waddle.test", "bob"))
        assertEquals(setOf("room@muc.waddle.test"), store.occupants.value.keys)
        assertEquals(setOf("Alice", "bob"), store.occupants.value["ROOM@muc.waddle.test"]?.keys)

        store.onPresence(occupant("ROOM@muc.waddle.test", "Alice", type = "unavailable"))
        store.onPresence(occupant("room@muc.waddle.test", "bob", type = "unavailable"))
        assertTrue(store.occupants.value.isEmpty())
    }

    @Test
    fun `a fresh session forgets contacts and occupants but keeps bots`() {
        store.onPresence(contact("alice@waddle.test/phone"))
        store.onPresence(occupant("room@muc.waddle.test", "bob", real = "bob@waddle.test/web"))
        store.onPresence(
            testPresence(
                from = "room@muc.waddle.test/helper",
                mucRole = WaddleMucRole.PARTICIPANT,
                mucJid = "Helper@waddle.test/bot",
                hats = listOf(WaddlePresenceHat(uri = HAT_URI_BOT, title = "Bot")),
            ),
        )

        store.resetForFreshSession()

        assertTrue(store.contacts.value.isEmpty())
        assertTrue(store.occupants.value.isEmpty())
        assertEquals(setOf("helper@waddle.test"), store.botJids.value)
        // Own JID survives too: our resources still never become contacts.
        store.onPresence(contact("me@waddle.test/laptop"))
        assertTrue(store.contacts.value.isEmpty())
    }
}
