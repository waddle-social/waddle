package social.waddle.android.client.store

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.StickerPack
import social.waddle.android.client.StickerPacksResult

/**
 * Session cache reconcile rules, in particular the non-loaded states:
 * post-Ok publish/retract updates must never fabricate a loaded cache
 * out of Unavailable/null — that would short-circuit the next load
 * with a silently incomplete answer.
 */
class StickerPackStoreTest {
    private fun pack(id: String) = StickerPack(
        id = id,
        name = "Penguins",
        summary = null,
        restricted = false,
        stickers = emptyList(),
    )

    @Test
    fun `publish merges into a ready cache under its id`() {
        val store = StickerPackStore()
        store.applyLoaded(listOf(pack("pack-1")))

        store.applyPublished(pack("pack-2"))

        assertEquals(
            StickerPacksResult.Ready(listOf(pack("pack-1"), pack("pack-2"))),
            store.packs.value,
        )
    }

    @Test
    fun `publish into the empty first-run state becomes ready`() {
        val store = StickerPackStore()
        store.applyLoaded(emptyList())

        store.applyPublished(pack("pack-1"))

        assertEquals(StickerPacksResult.Ready(listOf(pack("pack-1"))), store.packs.value)
    }

    @Test
    fun `publish leaves an unloaded cache unloaded`() {
        val store = StickerPackStore()

        store.applyPublished(pack("pack-1"))
        assertEquals(null, store.packs.value)
        assertFalse(store.isLoaded)

        store.applyUnavailable()
        store.applyPublished(pack("pack-1"))
        assertEquals(StickerPacksResult.Unavailable, store.packs.value)
        assertFalse("the next load must still refetch", store.isLoaded)
    }

    @Test
    fun `remove drops the pack and the last removal empties the cache`() {
        val store = StickerPackStore()
        store.applyLoaded(listOf(pack("pack-1"), pack("pack-2")))

        store.applyRemoved("pack-1")
        assertEquals(StickerPacksResult.Ready(listOf(pack("pack-2"))), store.packs.value)

        store.applyRemoved("pack-2")
        assertEquals(StickerPacksResult.Empty, store.packs.value)
        assertTrue(store.isLoaded)
    }

    @Test
    fun `remove leaves an unloaded cache unloaded`() {
        val store = StickerPackStore()

        store.applyRemoved("pack-1")
        assertEquals(null, store.packs.value)
        assertFalse(store.isLoaded)

        store.applyUnavailable()
        store.applyRemoved("pack-1")
        assertEquals(StickerPacksResult.Unavailable, store.packs.value)
        assertFalse("the next load must still refetch", store.isLoaded)
    }
}
