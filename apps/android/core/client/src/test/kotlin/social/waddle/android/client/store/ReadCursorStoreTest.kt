package social.waddle.android.client.store

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ReadCursorStoreTest {
    private val store = ReadCursorStore()

    @Test
    fun `advance records the cursor keyed by bare jid`() {
        assertTrue(store.advance("alice@waddle.test/phone", "s1"))

        assertEquals("s1", store.cursor("alice@waddle.test"))
        assertEquals("s1", store.cursor("alice@waddle.test/tablet"))
        assertEquals(mapOf("alice@waddle.test" to "s1"), store.cursors.value)
    }

    @Test
    fun `advancing to the same id is a deduped no-op`() {
        assertTrue(store.advance("alice@waddle.test", "s1"))
        assertFalse("equality-deduped", store.advance("alice@waddle.test", "s1"))
        assertTrue("a different id writes again", store.advance("alice@waddle.test", "s2"))

        assertEquals("s2", store.cursor("alice@waddle.test"))
    }

    @Test
    fun `conversations keep independent cursors`() {
        store.advance("alice@waddle.test", "s1")
        store.advance("general@muc.waddle.test", "room-7")

        assertEquals("s1", store.cursor("alice@waddle.test"))
        assertEquals("room-7", store.cursor("general@muc.waddle.test"))
    }

    @Test
    fun `compare-and-advance swaps only when the cursor is still expected`() {
        store.advance("alice@waddle.test", "s1")

        assertTrue(store.compareAndAdvance("alice@waddle.test", expected = "s1", stanzaId = "s2"))
        assertEquals("s2", store.cursor("alice@waddle.test"))

        assertFalse(
            "a stale snapshot must not regress the cursor",
            store.compareAndAdvance("alice@waddle.test", expected = "s1", stanzaId = "s3"),
        )
        assertEquals("s2", store.cursor("alice@waddle.test"))
    }

    @Test
    fun `compare-and-advance from an absent cursor expects null`() {
        assertFalse(store.compareAndAdvance("alice@waddle.test", expected = "s1", stanzaId = "s2"))
        assertNull(store.cursor("alice@waddle.test"))

        assertTrue(store.compareAndAdvance("alice@waddle.test", expected = null, stanzaId = "s1"))
        assertEquals("s1", store.cursor("alice@waddle.test"))
    }

    @Test
    fun `clear drops every cursor`() {
        store.advance("alice@waddle.test", "s1")
        store.advance("bob@waddle.test", "s2")

        store.clear()

        assertTrue(store.cursors.value.isEmpty())
        assertNull(store.cursor("alice@waddle.test"))
    }
}
