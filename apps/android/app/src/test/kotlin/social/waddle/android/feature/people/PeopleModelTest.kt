package social.waddle.android.feature.people

import org.junit.Assert.assertEquals
import org.junit.Test
import social.waddle.android.client.store.DirectoryPerson
import social.waddle.android.client.store.PresenceShow

class PeopleModelTest {
    private fun person(
        name: String,
        show: PresenceShow = PresenceShow.AVAILABLE,
        inHuddle: Boolean = false,
        speaking: Boolean = false,
    ) = PersonRow(
        jid = "${name.lowercase()}@waddle.test",
        name = name,
        show = show,
        inHuddle = inHuddle,
        speaking = speaking,
        huddleRoomName = if (inHuddle) "storage" else null,
    )

    @Test
    fun `groups split huddle, around and away, each in people-rail order`() {
        val groups = peopleGroupsOf(
            listOf(
                person("zed", PresenceShow.OFFLINE),
                person("ada", PresenceShow.DND),
                person("bo"),
                person("cy", PresenceShow.AWAY),
                person("dee", PresenceShow.XA),
                person("eve", PresenceShow.AWAY, inHuddle = true),
                person("fay", inHuddle = true, speaking = true),
                person("gus", PresenceShow.DND, inHuddle = true),
            ),
        )
        assertEquals(listOf("fay", "eve", "gus"), groups.getValue(PeopleGroup.HUDDLE).map { it.name })
        // Available before do not disturb, then by name.
        assertEquals(listOf("bo", "ada"), groups.getValue(PeopleGroup.AROUND).map { it.name })
        assertEquals(listOf("cy", "dee", "zed"), groups.getValue(PeopleGroup.AWAY).map { it.name })
    }

    @Test
    fun `every group is present even when empty`() {
        val groups = peopleGroupsOf(emptyList())
        assertEquals(PeopleGroup.entries.toList(), groups.keys.toList())
    }

    @Test
    fun `search matches name or jid, ignoring case`() {
        val people = listOf(person("Mara"), person("Sol"), person("Kai").copy(jid = "kai@elsewhere.test"))
        assertEquals(listOf("Mara"), peopleGroupsOf(people, "mar").values.flatten().map { it.name })
        assertEquals(listOf("Kai"), peopleGroupsOf(people, "ELSEWHERE").values.flatten().map { it.name })
    }

    @Test
    fun `name ties break by jid`() {
        val a = person("sam").copy(jid = "sam@a.test")
        val b = person("Sam").copy(jid = "sam@b.test")
        assertEquals(listOf("sam@a.test", "sam@b.test"), listOf(b, a).sortedWith(PersonOrder).map { it.jid })
    }

    @Test
    fun `strip puts huddles first, then available, then dnd, and leaves away out`() {
        val strip = whoIsHereOf(
            listOf(
                person("ada", PresenceShow.DND),
                person("bo"),
                person("cy", PresenceShow.AWAY),
                person("dee", PresenceShow.AWAY, inHuddle = true),
                person("eve", PresenceShow.OFFLINE),
                person("fay", inHuddle = true, speaking = true),
            ),
        )
        assertEquals(listOf("fay", "dee", "bo", "ada"), strip.map { it.name })
        assertEquals(4, aroundCountOf(strip))
    }

    @Test
    fun `rows resolve huddle room names and speakers by bare jid`() {
        val rows = personRowsOf(
            people = listOf(
                DirectoryPerson(
                    bareJid = "mara@waddle.test",
                    displayName = "mara",
                    show = PresenceShow.AVAILABLE,
                    huddleRoomJid = "storage@muc.waddle.test",
                    roomJids = listOf("storage@muc.waddle.test"),
                ),
                DirectoryPerson(
                    bareJid = "sol@waddle.test",
                    displayName = "sol",
                    show = PresenceShow.AWAY,
                    huddleRoomJid = "gone@muc.waddle.test",
                    roomJids = emptyList(),
                ),
            ),
            roomNames = mapOf("storage@muc.waddle.test" to "Storage"),
            speaking = setOf("Mara@Waddle.test/phone"),
        )
        assertEquals("Storage", rows[0].huddleRoomName)
        assertEquals(true, rows[0].speaking)
        assertEquals("gone", rows[1].huddleRoomName)
        assertEquals(false, rows[1].speaking)
    }
}
