package social.waddle.android.feature.home

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.feature.rooms.HuddleParticipant
import social.waddle.android.feature.rooms.RoomActivity

class HomeModelTest {
    private fun room(
        name: String,
        huddle: Int = 0,
        mentions: Int = 0,
        unread: Int = 0,
        threadUnread: Int = 0,
        lastUpdated: Long? = null,
        muted: Boolean = false,
        youAreInHuddle: Boolean = false,
    ) = RoomActivity(
        roomJid = "$name@muc.waddle.test",
        name = name,
        joined = true,
        here = 0,
        huddle = List(huddle) { HuddleParticipant("p$it", null) },
        mentions = mentions,
        unread = unread,
        threadUnread = threadUnread,
        muted = muted,
        lastUpdated = lastUpdated,
        youAreInHuddle = youAreInHuddle,
    )

    @Test
    fun `time of day follows the web boundaries`() {
        val expected = mapOf(
            0 to TimeOfDay.NIGHT,
            4 to TimeOfDay.NIGHT,
            5 to TimeOfDay.MORNING,
            10 to TimeOfDay.MORNING,
            11 to TimeOfDay.DAY,
            16 to TimeOfDay.DAY,
            17 to TimeOfDay.EVENING,
            21 to TimeOfDay.EVENING,
            22 to TimeOfDay.NIGHT,
            23 to TimeOfDay.NIGHT,
        )
        expected.forEach { (hour, time) -> assertEquals("hour $hour", time, TimeOfDay.of(hour)) }
    }

    @Test
    fun `summary drops zero segments and keeps the web order`() {
        val summary = HomeSummary(mentions = 2, unread = 0, threadReplies = 1, activeCalls = 0, around = 3)
        val parts = summaryPartsOf(summary)
        assertEquals(
            listOf(SummaryPart.MENTIONS to 2, SummaryPart.THREAD_REPLIES to 1, SummaryPart.AROUND to 3),
            parts,
        )
        assertTrue(isQuiet(HomeSummary()))
        assertFalse(isQuiet(HomeSummary(around = 1)))
    }

    @Test
    fun `summary counts loud unread plus dms and live rooms`() {
        val summary = summaryOf(
            rooms = listOf(
                room("a", mentions = 1, unread = 3, threadUnread = 2),
                room("b", unread = 7, muted = true),
                room("c", huddle = 2),
            ),
            dmUnread = 4,
            around = 5,
        )
        assertEquals(HomeSummary(mentions = 1, unread = 7, threadReplies = 2, activeCalls = 1, around = 5), summary)
    }

    @Test
    fun `happening now is live huddles then the busiest rooms, at most three, needs-you rooms apart`() {
        val sections = homeSectionsOf(
            listOf(
                room("quiet"),
                room("mention", mentions = 1),
                room("thread", threadUnread = 2),
                room("busy", unread = 9),
                room("busier", unread = 12),
                room("newest", unread = 1, lastUpdated = 99),
                room("small-huddle", huddle = 1),
                room("big-huddle", huddle = 4),
            ),
        )
        assertEquals(listOf("big-huddle", "small-huddle"), sections.live.map { it.name })
        assertEquals(listOf("busier"), sections.busiest.map { it.name })
        assertEquals(listOf("mention", "thread"), sections.needsYou.map { it.name })
    }

    @Test
    fun `busiest fills to three when nothing is live`() {
        val sections = homeSectionsOf(
            listOf(room("a", unread = 1), room("b", unread = 2), room("c", unread = 3), room("d", unread = 4)),
        )
        assertTrue(sections.live.isEmpty())
        assertEquals(listOf("d", "c", "b"), sections.busiest.map { it.name })
    }

    @Test
    fun `hero acts on your own call first, then a joinable huddle, then the busiest room`() {
        val mine = room("mine", huddle = 2, youAreInHuddle = true)
        val other = room("other", huddle = 3)
        val busy = room("busy", unread = 2)
        assertEquals(HeroAction.Return(mine), heroActionOf(listOf(other, mine), listOf(busy)))
        assertEquals(HeroAction.Join(other), heroActionOf(listOf(other), listOf(busy)))
        assertEquals(HeroAction.JumpInto(busy), heroActionOf(emptyList(), listOf(busy)))
        assertEquals(HeroAction.Browse, heroActionOf(emptyList(), emptyList()))
        val blocked = other.copy(busyElsewhere = true)
        assertEquals(HeroAction.JumpInto(busy), heroActionOf(listOf(blocked), listOf(busy)))
    }

    @Test
    fun `participant preview names two then counts`() {
        assertEquals("", participantPreviewOf(emptyList()))
        assertEquals("mara", participantPreviewOf(listOf("mara")))
        assertEquals("mara, sol", participantPreviewOf(listOf("mara", " sol ")))
        assertEquals("mara, sol +2", participantPreviewOf(listOf("mara", "sol", "kai", "noor")))
    }

    @Test
    fun `greeting name prefers the nickname, then a first name, then the account`() {
        assertEquals("Dave", greetingNameOf("Dave", "David Flanagan", "rawkode"))
        assertEquals("David", greetingNameOf(" ", "David Flanagan", "rawkode"))
        assertEquals("rawkode", greetingNameOf(null, null, "rawkode"))
        assertNull(greetingNameOf(null, "", " "))
    }

    @Test
    fun `stable order keeps known keys in place and appends arrivals`() {
        val previous = listOf("b", "a", "c")
        assertEquals(listOf("b", "a", "c"), stableOrderOf(previous, listOf("a", "b", "c")) { it })
        assertEquals(listOf("b", "c", "d"), stableOrderOf(previous, listOf("d", "c", "b")) { it })
        assertEquals(listOf("x", "y"), stableOrderOf(emptyList(), listOf("x", "y")) { it })
    }
}
