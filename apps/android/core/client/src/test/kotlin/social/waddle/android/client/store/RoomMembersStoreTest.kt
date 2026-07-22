package social.waddle.android.client.store

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.client.ffi.WaddleMucAffiliation
import social.waddle.client.ffi.WaddleRoomMemberEntry

class RoomMembersStoreTest {
    private val store = RoomMembersStore()
    private val room = "general@muc.waddle.test"

    private fun entry(jid: String, affiliation: WaddleMucAffiliation) = WaddleRoomMemberEntry(
        jid = jid,
        affiliation = affiliation,
        nick = null,
        reason = null,
    )

    @Test
    fun `loaded snapshot replaces the previous list`() {
        store.applyLoaded(room, listOf(entry("alice@waddle.test", WaddleMucAffiliation.OWNER)))
        store.applyLoaded(room, listOf(entry("bob@waddle.test", WaddleMucAffiliation.MEMBER)))

        val state = store.rooms.value.getValue(room)
        assertEquals(MemberListStatus.LOADED, state.status)
        assertEquals(listOf("bob@waddle.test"), state.members.map { it.jid })
    }

    @Test
    fun `unavailable keeps the last synced members`() {
        // Web parity: when every affiliation query fails, the screen
        // degrades to "showing last synced members" instead of
        // flashing empty.
        store.applyLoaded(room, listOf(entry("alice@waddle.test", WaddleMucAffiliation.OWNER)))
        store.applyUnavailable(room)

        val state = store.rooms.value.getValue(room)
        assertEquals(MemberListStatus.UNAVAILABLE, state.status)
        assertEquals(listOf("alice@waddle.test"), state.members.map { it.jid })
    }

    @Test
    fun `loading keeps the previous snapshot visible`() {
        store.applyLoaded(room, listOf(entry("alice@waddle.test", WaddleMucAffiliation.OWNER)))
        store.markLoading(room)

        val state = store.rooms.value.getValue(room)
        assertEquals(MemberListStatus.LOADING, state.status)
        assertEquals(1, state.members.size)
    }

    @Test
    fun `clear wipes all rooms`() {
        store.applyLoaded(room, listOf(entry("alice@waddle.test", WaddleMucAffiliation.OWNER)))
        store.clear()
        assertTrue(store.rooms.value.isEmpty())
    }
}
