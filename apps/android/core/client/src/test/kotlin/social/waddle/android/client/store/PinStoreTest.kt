package social.waddle.android.client.store

import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.client.ffi.WaddlePinAction
import social.waddle.client.ffi.WaddlePinEntry
import social.waddle.client.ffi.WaddlePinEvent
import social.waddle.client.ffi.WaddlePinPreview

class PinStoreTest {
    private val store = PinStore()

    private fun preview() = WaddlePinPreview(
        authorJid = "alice@waddle.test",
        authorNick = "alice",
        text = "pinned text",
        messageTimestamp = "2026-07-15T10:00:00Z",
    )

    private fun entry(targetStanzaId: String) = WaddlePinEntry(
        targetStanzaId = targetStanzaId,
        pinnerJid = "alice@waddle.test",
        pinnedAt = "2026-07-15T11:00:00Z",
        preview = preview(),
    )

    private fun event(action: WaddlePinAction, targetStanzaId: String) = WaddlePinEvent(
        action = action,
        targetStanzaId = targetStanzaId,
        by = "alice@waddle.test",
        reason = null,
        preview = if (action == WaddlePinAction.PINNED) preview() else null,
    )

    @Test
    fun `pin events add and unpin events remove, keyed by bare room jid`() = runTest {
        store.onPinEvent("general@muc.waddle.test/alice", event(WaddlePinAction.PINNED, "s1"))
        store.onPinEvent("general@muc.waddle.test", event(WaddlePinAction.PINNED, "s2"))

        assertEquals(setOf("s1", "s2"), store.pinnedIds("general@muc.waddle.test").first())

        store.onPinEvent("general@muc.waddle.test", event(WaddlePinAction.UNPINNED, "s1"))
        assertEquals(setOf("s2"), store.pinnedIds("general@muc.waddle.test").first())
    }

    @Test
    fun `unpinning something never pinned is a no-op`() = runTest {
        store.onPinEvent("general@muc.waddle.test", event(WaddlePinAction.UNPINNED, "ghost"))

        assertTrue(store.pinnedIds("general@muc.waddle.test").first().isEmpty())
    }

    @Test
    fun `rooms keep isolated pin sets`() = runTest {
        store.onPinEvent("general@muc.waddle.test", event(WaddlePinAction.PINNED, "s1"))
        store.onPinEvent("dev@muc.waddle.test", event(WaddlePinAction.PINNED, "s2"))

        assertEquals(setOf("s1"), store.pinnedIds("general@muc.waddle.test").first())
        assertEquals(setOf("s2"), store.pinnedIds("dev@muc.waddle.test").first())
    }

    @Test
    fun `seed replaces the room's set when no event raced the fetch`() = runTest {
        store.onPinEvent("general@muc.waddle.test", event(WaddlePinAction.PINNED, "old"))

        val version = store.eventVersion("general@muc.waddle.test")
        store.seed("general@muc.waddle.test", listOf(entry("s1"), entry("s2")), version)

        assertEquals(setOf("s1", "s2"), store.pinnedIds("general@muc.waddle.test").first())
    }

    @Test
    fun `a snapshot raced by a live event is discarded as stale`() = runTest {
        val version = store.eventVersion("general@muc.waddle.test")
        store.onPinEvent("general@muc.waddle.test", event(WaddlePinAction.PINNED, "live"))

        store.seed("general@muc.waddle.test", listOf(entry("stale")), version)

        assertEquals(
            "the newer wire state must win",
            setOf("live"),
            store.pinnedIds("general@muc.waddle.test").first(),
        )
    }

    @Test
    fun `event versions tick per room with bare jid normalization`() {
        assertEquals(0L, store.eventVersion("general@muc.waddle.test"))

        store.onPinEvent("general@muc.waddle.test/alice", event(WaddlePinAction.PINNED, "s1"))
        store.onPinEvent("general@muc.waddle.test", event(WaddlePinAction.UNPINNED, "s1"))

        assertEquals(2L, store.eventVersion("general@muc.waddle.test/whatever"))
        assertEquals("other rooms untouched", 0L, store.eventVersion("dev@muc.waddle.test"))
    }

    @Test
    fun `clear wipes pins and event versions`() = runTest {
        store.onPinEvent("general@muc.waddle.test", event(WaddlePinAction.PINNED, "s1"))

        store.clear()

        assertTrue(store.pinned.value.isEmpty())
        assertEquals(0L, store.eventVersion("general@muc.waddle.test"))
        // A post-clear fetch seeds against the reset version counter.
        store.seed("general@muc.waddle.test", listOf(entry("s9")), fetchedAtVersion = 0L)
        assertEquals(setOf("s9"), store.pinnedIds("general@muc.waddle.test").first())
    }
}
