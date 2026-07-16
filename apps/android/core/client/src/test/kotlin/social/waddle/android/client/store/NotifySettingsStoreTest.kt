package social.waddle.android.client.store

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.client.ffi.WaddleBookmarkItem
import social.waddle.client.ffi.WaddleDmBookmarkItem
import social.waddle.client.ffi.WaddleNotifyMode

class NotifySettingsStoreTest {
    private val store = NotifySettingsStore()

    private fun roomItem(
        jid: String,
        mode: WaddleNotifyMode?,
        richPayloadOptIn: Boolean = false,
    ) = WaddleBookmarkItem(
        jid = jid,
        name = null,
        autojoin = false,
        notifyMode = mode,
        richPayloadOptIn = richPayloadOptIn,
    )

    private fun dmItem(
        jid: String,
        mode: WaddleNotifyMode?,
        richPayloadOptIn: Boolean = false,
    ) = WaddleDmBookmarkItem(jid = jid, notifyMode = mode, richPayloadOptIn = richPayloadOptIn)

    @Test
    fun `xep-0492 section 3 defaults resolve per conversation kind`() {
        // "always" for direct chats AND private group chats,
        // "on-mention" for public group chats.
        assertEquals(
            WaddleNotifyMode.ALWAYS,
            store.modeFor("alice@waddle.test", ConversationKind.DIRECT_CHAT),
        )
        assertEquals(
            WaddleNotifyMode.ALWAYS,
            store.modeFor("room@muc.waddle.test", ConversationKind.PRIVATE_GROUP),
        )
        assertEquals(
            WaddleNotifyMode.ON_MENTION,
            store.modeFor("room@muc.waddle.test", ConversationKind.PUBLIC_GROUP),
        )
    }

    @Test
    fun `a fallbackless entry still resolves to the section 3 default`() = runTest {
        // A bookmark whose <notify/> has only identity-scoped siblings
        // surfaces with a null mode — the default still applies.
        store.hydrate(
            fetchRoomBookmarks = { listOf(roomItem("room@muc.waddle.test", mode = null)) },
            fetchDmBookmarks = { emptyList() },
        )

        assertEquals(
            WaddleNotifyMode.ALWAYS,
            store.modeFor("room@muc.waddle.test", ConversationKind.PRIVATE_GROUP),
        )
    }

    @Test
    fun `hydrate merges the room and dm carriers`() = runTest {
        store.hydrate(
            fetchRoomBookmarks = {
                listOf(roomItem("room@muc.waddle.test", WaddleNotifyMode.NEVER, richPayloadOptIn = true))
            },
            fetchDmBookmarks = { listOf(dmItem("bob@waddle.test", WaddleNotifyMode.ON_MENTION)) },
        )

        assertEquals(
            WaddleNotifyMode.NEVER,
            store.modeFor("room@muc.waddle.test", ConversationKind.PRIVATE_GROUP),
        )
        assertTrue(store.richPayloadOptIn("room@muc.waddle.test"))
        assertEquals(
            WaddleNotifyMode.ON_MENTION,
            store.modeFor("bob@waddle.test", ConversationKind.DIRECT_CHAT),
        )
        assertFalse(store.richPayloadOptIn("bob@waddle.test"))
    }

    @Test
    fun `modeFor normalizes full jids to bare`() = runTest {
        store.hydrate(
            fetchRoomBookmarks = { emptyList() },
            fetchDmBookmarks = { listOf(dmItem("bob@waddle.test", WaddleNotifyMode.NEVER)) },
        )

        assertEquals(
            WaddleNotifyMode.NEVER,
            store.modeFor("bob@waddle.test/phone", ConversationKind.DIRECT_CHAT),
        )
    }

    @Test
    fun `a clear during hydrate discards the stale snapshot`() = runTest {
        val gate = CompletableDeferred<List<WaddleBookmarkItem>>()
        val hydrate = launch {
            store.hydrate(
                fetchRoomBookmarks = { gate.await() },
                fetchDmBookmarks = { emptyList() },
            )
        }
        runCurrent()

        // Logout (or account switch) wipes the store mid-fetch; the
        // pre-logout snapshot must not resurrect into the next session.
        store.clear()
        gate.complete(listOf(roomItem("room@muc.waddle.test", WaddleNotifyMode.NEVER)))
        hydrate.join()

        assertTrue(store.entries.value.isEmpty())
    }

    @Test
    fun `a local set during hydrate wins over the stale snapshot`() = runTest {
        val gate = CompletableDeferred<List<WaddleBookmarkItem>>()
        val hydrate = launch {
            store.hydrate(
                fetchRoomBookmarks = { gate.await() },
                fetchDmBookmarks = { emptyList() },
            )
        }
        runCurrent()

        // The user picks a mode while the fetch is in flight: the
        // published value is newer than whatever the fetch snapshotted.
        store.applyRoomUpdate(roomItem("room@muc.waddle.test", WaddleNotifyMode.NEVER))
        gate.complete(listOf(roomItem("room@muc.waddle.test", WaddleNotifyMode.ALWAYS)))
        hydrate.join()

        assertEquals(
            WaddleNotifyMode.NEVER,
            store.modeFor("room@muc.waddle.test", ConversationKind.PRIVATE_GROUP),
        )
    }

    @Test
    fun `concurrent hydrates run once`() = runTest {
        val gate = CompletableDeferred<List<WaddleBookmarkItem>>()
        var fetches = 0
        val first = launch {
            store.hydrate(
                fetchRoomBookmarks = {
                    fetches += 1
                    gate.await()
                },
                fetchDmBookmarks = { emptyList() },
            )
        }
        runCurrent()

        // Re-entrant hydrate while the first is parked: no-op.
        store.hydrate(
            fetchRoomBookmarks = {
                fetches += 1
                emptyList()
            },
            fetchDmBookmarks = { emptyList() },
        )
        gate.complete(emptyList())
        first.join()

        assertEquals(1, fetches)
    }

    @Test
    fun `set-verb reconciles update and remove entries`() = runTest {
        store.applyDmUpdate(dmItem("bob@waddle.test", WaddleNotifyMode.NEVER))
        assertEquals(
            WaddleNotifyMode.NEVER,
            store.modeFor("bob@waddle.test", ConversationKind.DIRECT_CHAT),
        )

        // The sparse DM node retracted the override: back to the default.
        store.applyDmRemoved("bob@waddle.test")
        assertEquals(
            WaddleNotifyMode.ALWAYS,
            store.modeFor("bob@waddle.test", ConversationKind.DIRECT_CHAT),
        )
    }

    @Test
    fun `clear wipes entries`() = runTest {
        store.applyRoomUpdate(roomItem("room@muc.waddle.test", WaddleNotifyMode.NEVER))
        store.clear()
        assertTrue(store.entries.value.isEmpty())
    }
}
