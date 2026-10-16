package social.waddle.android.feature.rooms

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.calls.CallKind
import social.waddle.android.client.calls.CallState
import social.waddle.android.client.calls.MucCallMedia
import social.waddle.android.client.store.InboxEntrySnapshot
import social.waddle.android.client.store.InboxKind
import social.waddle.android.client.store.NotifySettingsEntry
import social.waddle.android.client.store.RoomHeadcount
import social.waddle.android.client.testChannel
import social.waddle.client.ffi.WaddleCallMedia
import social.waddle.client.ffi.WaddleLiveKitJoin
import social.waddle.client.ffi.WaddleNotifyMode

class RoomActivityTest {
    private val now = 1_000_000_000L

    private fun room(
        name: String,
        huddle: Int = 0,
        mentions: Int = 0,
        unread: Int = 0,
        lastUpdated: Long? = null,
        here: Int? = null,
        joined: Boolean = here != null,
        muted: Boolean = false,
    ) = RoomActivity(
        roomJid = "$name@muc.waddle.test",
        name = name,
        joined = joined,
        here = here,
        huddle = List(huddle) { HuddleParticipant("p$it", null) },
        mentions = mentions,
        unread = unread,
        muted = muted,
        lastUpdated = lastUpdated,
    )

    @Test
    fun `rooms sort by huddle size, mentions, unread, recency, then name`() {
        val rooms = listOf(
            room("zeta"),
            room("alpha"),
            room("recent", lastUpdated = now),
            room("unread", unread = 9),
            room("mentioned", mentions = 1),
            room("small-huddle", huddle = 1),
            room("big-huddle", huddle = 3),
        )
        assertEquals(
            listOf("big-huddle", "small-huddle", "mentioned", "unread", "recent", "alpha", "zeta"),
            rooms.sortedWith(RoomActivityOrder).map { it.name },
        )
    }

    @Test
    fun `name ties break case-insensitively, then by jid`() {
        val upper = room("Design").copy(roomJid = "b@muc")
        val lower = room("design").copy(roomJid = "a@muc")
        val other = room("Alpha")
        assertEquals(
            listOf("Alpha", "design", "Design"),
            listOf(upper, other, lower).sortedWith(RoomActivityOrder).map { it.name },
        )
    }

    @Test
    fun `muted unread does not lift a room`() {
        val muted = room("muted", unread = 50, muted = true)
        val loud = room("loud", unread = 1)
        assertEquals(listOf("loud", "muted"), listOf(muted, loud).sortedWith(RoomActivityOrder).map { it.name })
    }

    @Test
    fun `kicker takes the first true thing`() {
        assertEquals(
            RoomKicker.Huddle(video = false, count = 2),
            roomKickerOf(room("a", huddle = 2, here = 5, mentions = 1), now),
        )
        assertEquals(RoomKicker.Here(5), roomKickerOf(room("a", here = 5, mentions = 1), now))
        assertEquals(RoomKicker.Mentions(2), roomKickerOf(room("a", here = 0, mentions = 2, unread = 4), now))
        assertEquals(RoomKicker.Unread(4), roomKickerOf(room("a", here = 0, unread = 4), now))
        assertEquals(RoomKicker.Active, roomKickerOf(room("a", here = 0, lastUpdated = now - 60_000), now))
        val stale = room("a", here = 0, lastUpdated = now - 2 * ACTIVE_WINDOW_MILLIS)
        assertEquals(RoomKicker.Quiet, roomKickerOf(stale, now))
    }

    @Test
    fun `video huddles say so`() {
        val video = room("a", huddle = 1).copy(video = true)
        assertEquals(RoomKicker.Huddle(video = true, count = 1), roomKickerOf(video, now))
    }

    @Test
    fun `a room this account is not in is never called quiet`() {
        assertEquals(RoomKicker.Room, roomKickerOf(room("a", joined = false), now))
        // What it does know (unread from the server inbox) it still says.
        assertEquals(RoomKicker.Unread(3), roomKickerOf(room("a", joined = false, unread = 3), now))
    }

    @Test
    fun `muted unread is not a kicker`() {
        assertEquals(RoomKicker.Quiet, roomKickerOf(room("a", here = 0, unread = 3, muted = true), now))
    }

    @Test
    fun `assembly joins counts, headcounts, inbox and the huddle`() {
        val general = "general@muc.waddle.test"
        val storage = "Storage@muc.waddle.test"
        val rooms = roomActivitiesOf(
            RoomInputs(
                channels = listOf(
                    testChannel(general),
                    testChannel(storage, name = "storage"),
                    testChannel("trio@muc.waddle.test", isGroupDm = true),
                ),
                joinedRooms = setOf(general, storage),
                headcounts = mapOf(
                    "general@muc.waddle.test" to RoomHeadcount(here = 4, inHuddle = 0),
                    "storage@muc.waddle.test" to RoomHeadcount(here = 2, inHuddle = 2),
                ),
                mentions = mapOf(general to 2),
                unread = mapOf(general to 5),
                threadUnread = mapOf(general to 1),
                inbox = mapOf(general to inbox(general, "shipped it", author = "$general/mara", at = 42L)),
                notify = mapOf(
                    general to NotifySettingsEntry(notifyMode = WaddleNotifyMode.NEVER, richPayloadOptIn = false),
                ),
                calls = RoomCallInputs(
                    participants = mapOf("storage@muc.waddle.test" to linkedSetOf("sol", "mara")),
                    owners = mapOf(
                        "storage@muc.waddle.test" to mapOf("sol" to "sol@waddle.test/phone", "mara" to null),
                    ),
                    media = mapOf("storage@muc.waddle.test" to MucCallMedia(audio = true, video = true)),
                ),
            ),
        )
        assertEquals(listOf("general", "storage"), rooms.map { it.name })
        val g = rooms[0]
        assertEquals(4, g.here)
        assertEquals(2, g.mentions)
        assertEquals(5, g.unread)
        assertEquals(0, g.loudUnread)
        assertTrue(g.muted)
        assertEquals(1, g.threadUnread)
        assertEquals("shipped it", g.preview)
        assertEquals("mara", g.previewAuthor)
        assertEquals(42L, g.lastUpdated)
        val s = rooms[1]
        assertEquals(listOf(HuddleParticipant("sol", "sol@waddle.test"), HuddleParticipant("mara", null)), s.huddle)
        assertTrue(s.video)
        assertTrue(s.canJoin)
    }

    @Test
    fun `unjoined rooms have no headcount`() {
        val rooms = roomActivitiesOf(
            inputs(channels = listOf(testChannel("lobby@muc.waddle.test")), joined = emptySet()),
        )
        assertFalse(rooms.single().joined)
        assertNull(rooms.single().here)
    }

    @Test
    fun `a call elsewhere blocks join, a call here returns`() {
        val roomJid = "storage@muc.waddle.test"
        val calls = RoomCallInputs(participants = mapOf(roomJid to setOf("sol")))
        val busy = roomActivitiesOf(
            inputs(
                channels = listOf(testChannel(roomJid)),
                calls = calls.copy(state = activeCall(peer = "other@muc.waddle.test")),
            ),
        ).single()
        assertTrue(busy.busyElsewhere)
        assertFalse(busy.canJoin)
        val here = roomActivitiesOf(
            inputs(channels = listOf(testChannel(roomJid)), calls = calls.copy(state = activeCall(peer = roomJid))),
        ).single()
        assertTrue(here.youAreInHuddle)
        assertFalse(here.busyElsewhere)
        assertFalse(here.canJoin)
    }

    private fun inputs(
        channels: List<social.waddle.client.ffi.WaddleChannel>,
        joined: Set<String> = channels.map { it.roomJid }.toSet(),
        calls: RoomCallInputs = RoomCallInputs(),
    ) = RoomInputs(
        channels = channels,
        joinedRooms = joined,
        headcounts = emptyMap(),
        mentions = emptyMap(),
        unread = emptyMap(),
        threadUnread = emptyMap(),
        inbox = emptyMap(),
        notify = emptyMap(),
        calls = calls,
    )

    private fun activeCall(peer: String) = CallState.Active(
        peer = peer,
        sid = "sid",
        media = WaddleCallMedia(audio = true, video = false),
        join = WaddleLiveKitJoin(url = "wss://lk", room = "r", identity = "i", token = "t"),
        kind = CallKind.MUC,
        selfNick = "david",
    )

    private fun inbox(partner: String, preview: String, author: String, at: Long) = InboxEntrySnapshot(
        partner = partner,
        kind = InboxKind.MUC,
        lastStanzaId = "s1",
        lastUpdated = at,
        unread = 0,
        preview = preview,
        threadId = null,
        threadTitle = null,
        replyCount = null,
        author = author,
    )
}
