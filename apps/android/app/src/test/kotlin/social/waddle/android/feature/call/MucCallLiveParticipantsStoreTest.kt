package social.waddle.android.feature.call

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * LiveKit participant projection + leave markers. The store mutates
 * from concurrent Dispatchers.Default coroutines, so every mutation
 * uses `MutableStateFlow.update` (atomic CAS-retry) — these tests pin
 * the read-modify-write semantics that contract preserves.
 */
class MucCallLiveParticipantsStoreTest {
    private val room = "room@muc.waddle.test"

    @Test
    fun `setParticipants dedupes identities and clears the pending leave marker`() {
        val store = MucCallLiveParticipantsStore()
        store.markLeaving(room, "me")

        store.setParticipants(
            room,
            listOf("Alice@waddle.test/a", "alice@waddle.test/a", "bob@waddle.test/b"),
        )

        assertEquals(
            listOf("alice@waddle.test/a", "bob@waddle.test/b"),
            store.participants.value[room],
        )
        assertNull(store.leavingRooms.value[room])
    }

    @Test
    fun `an empty snapshot drops the room so the Muji view re-engages`() {
        val store = MucCallLiveParticipantsStore()
        store.setParticipants(room, listOf("alice@waddle.test/a"))

        store.setParticipants(room, emptyList())

        assertNull(store.participants.value[room])
    }

    @Test
    fun `independent rooms never clobber each other's entries`() {
        val store = MucCallLiveParticipantsStore()
        val other = "other@muc.waddle.test"

        store.setParticipants(room, listOf("alice@waddle.test/a"))
        store.setParticipants(other, listOf("bob@waddle.test/b"))
        store.markLeaving(room, "me")
        store.markLeaving(other, "me-too")
        store.clearRoom(room)
        store.clearLeaving(room)

        assertNull(store.participants.value[room])
        assertEquals(listOf("bob@waddle.test/b"), store.participants.value[other])
        assertNull(store.leavingRooms.value[room])
        assertEquals("me-too", store.leavingRooms.value[other])
    }
}
