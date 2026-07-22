package social.waddle.android.client.store

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Test
import social.waddle.android.client.testAvatar
import social.waddle.android.client.testTune
import social.waddle.android.client.testVcard4
import social.waddle.client.ffi.WaddleActivity
import social.waddle.client.ffi.WaddleMood
import social.waddle.client.ffi.WaddlePepProfile

class ProfileStoreTest {
    private val store = ProfileStore()

    @Test
    fun `avatars cache by owner and item id and track the current one`() {
        val first = testAvatar(id = "id-1", data = byteArrayOf(1))
        val second = testAvatar(id = "id-2", data = byteArrayOf(2))
        store.onAvatar(first)
        store.onAvatar(second)

        assertSame(first, store.cachedAvatar("alice@waddle.test", "id-1"))
        assertSame(second, store.cachedAvatar("alice@waddle.test", "id-2"))
        assertSame(second, store.avatars.value["alice@waddle.test"])
    }

    @Test
    fun `full jids collapse to the bare owner`() {
        store.onAvatar(testAvatar(jid = "alice@waddle.test/phone", id = "id-1"))
        assertEquals("id-1", store.cachedAvatar("alice@waddle.test/tablet", "id-1")?.id)
        assertEquals("id-1", store.avatars.value["alice@waddle.test"]?.id)
    }

    @Test
    fun `clearAvatar drops the current avatar but keeps the id cache`() {
        val avatar = testAvatar(id = "id-1")
        store.onAvatar(avatar)
        store.clearAvatar("alice@waddle.test")

        assertNull(store.avatars.value["alice@waddle.test"])
        // A later metadata notification re-advertising id-1 must still
        // hit the cache (XEP-0084 §4.2: never re-fetch a held id).
        assertSame(avatar, store.cachedAvatar("alice@waddle.test", "id-1"))
    }

    @Test
    fun `setSelfStatus seeds all three status flows`() {
        store.setSelfStatus(
            WaddlePepProfile(
                mood = WaddleMood(kind = "happy", text = null),
                activity = WaddleActivity(general = "working", specific = "coding", text = null),
                tune = testTune(),
            ),
        )
        assertEquals("happy", store.selfMood.value?.kind)
        assertEquals("working", store.selfActivity.value?.general)
        assertEquals("Come Together", store.selfTune.value?.title)
    }

    @Test
    fun `clear wipes profile state and the avatar cache`() {
        store.setSelfVcard(testVcard4())
        store.setSelfMood(WaddleMood(kind = "happy", text = null))
        store.onAvatar(testAvatar(id = "id-1"))

        store.clear()

        assertNull(store.selfVcard.value)
        assertNull(store.selfMood.value)
        assertNull(store.selfActivity.value)
        assertNull(store.selfTune.value)
        assertEquals(emptyMap<String, Any>(), store.avatars.value)
        assertNull(store.cachedAvatar("alice@waddle.test", "id-1"))
    }
}
