package social.waddle.android.feature.people

import social.waddle.android.client.normalizedBareJid
import social.waddle.android.client.store.DirectoryPerson
import social.waddle.android.client.store.PresenceShow

/** One person as the People tab and the who-is-here strip show them. */
data class PersonRow(
    /** Normalised bare JID: the DM target and the stable key. */
    val jid: String,
    val name: String,
    val show: PresenceShow,
    /** The room whose huddle they are in, by its display name; `null` when not in one. */
    val huddleRoomName: String? = null,
    val inHuddle: Boolean = false,
    /** Only ever true for the call this device is in. */
    val speaking: Boolean = false,
)

/** People tab sections, top to bottom. */
enum class PeopleGroup {
    /** In a huddle (any presence): live, ember. */
    HUDDLE,

    /** Available or do not disturb: here, reachable or not. */
    AROUND,

    /** Away, extended away, offline: collapsed by default. */
    AWAY,
}

/** [people] (the directory: no self, no bots) as rows, with room names and speakers resolved. */
fun personRowsOf(
    people: List<DirectoryPerson>,
    roomNames: Map<String, String>,
    speaking: Set<String>,
): List<PersonRow> {
    val speakers = speaking.mapTo(HashSet(), ::normalizedBareJid)
    return people.map { person ->
        PersonRow(
            jid = person.bareJid,
            name = person.displayName,
            show = person.show,
            huddleRoomName = person.huddleRoomJid?.let { room -> roomNames[room] ?: room.substringBefore('@') },
            inHuddle = person.inHuddle,
            speaking = person.bareJid in speakers,
        )
    }
}

fun groupOf(person: PersonRow): PeopleGroup = when {
    person.inHuddle || person.speaking -> PeopleGroup.HUDDLE
    person.show == PresenceShow.AVAILABLE || person.show == PresenceShow.DND -> PeopleGroup.AROUND
    else -> PeopleGroup.AWAY
}

/**
 * Web people-rail order: speaking, in a huddle, available, away, do not
 * disturb, offline, then name and JID so equal people never swap.
 */
val PersonOrder: Comparator<PersonRow> =
    compareBy<PersonRow> { if (it.speaking) 0 else 1 }
        .thenBy { if (it.inHuddle) 0 else 1 }
        .thenBy { showRank(it.show) }
        .thenBy(String.CASE_INSENSITIVE_ORDER) { it.name }
        .thenBy { it.jid }

private fun showRank(show: PresenceShow): Int = when (show) {
    PresenceShow.AVAILABLE -> 0
    PresenceShow.AWAY -> 1
    PresenceShow.XA -> 2
    PresenceShow.DND -> 3
    PresenceShow.OFFLINE -> 4
}

/** Every group in tab order (empty ones included), each sorted by [PersonOrder]; [query] matches name or JID. */
fun peopleGroupsOf(people: List<PersonRow>, query: String = ""): Map<PeopleGroup, List<PersonRow>> {
    val needle = query.trim()
    val matching = if (needle.isEmpty()) {
        people
    } else {
        people.filter { it.name.contains(needle, ignoreCase = true) || it.jid.contains(needle, ignoreCase = true) }
    }
    val grouped = matching.sortedWith(PersonOrder).groupBy(::groupOf)
    return PeopleGroup.entries.associateWith { grouped[it].orEmpty() }
}

/**
 * The Home who-is-here strip: people in a huddle first (speakers
 * first among them), then everyone available, then do not disturb.
 * Away and offline people are not "here".
 */
fun whoIsHereOf(people: List<PersonRow>): List<PersonRow> =
    people.filter { it.inHuddle || it.speaking || it.show == PresenceShow.AVAILABLE || it.show == PresenceShow.DND }
        .filter { it.show != PresenceShow.OFFLINE }
        .sortedWith(PersonOrder)

/** Around for the Home kicker and summary: here and reachable or not, huddles included. */
fun aroundCountOf(people: List<PersonRow>): Int = whoIsHereOf(people).size
