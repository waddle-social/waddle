package social.waddle.android.client.store

import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.conflate
import kotlinx.coroutines.flow.transform
import social.waddle.android.client.hasBotHat
import social.waddle.android.client.normalizedBareJid
import social.waddle.client.ffi.WaddlePresence
import social.waddle.client.ffi.WaddleRoomBot

/**
 * One person the UI can show: someone in a joined room whose room
 * exposes their real JID, or an online contact. Immutable.
 */
data class DirectoryPerson(
    /** [normalizedBareJid] of the person; the dedupe key. */
    val bareJid: String,
    /** Their room nick (from the first room by JID), else their localpart. */
    val displayName: String,
    /** Most-available Show across their occupancies and contact resources. */
    val show: PresenceShow,
    /**
     * The (normalised) room whose XEP-0272 Muji call they are in, or
     * `null`. The first by room JID if they are in several.
     */
    val huddleRoomJid: String?,
    /** Normalised JIDs of the joined rooms they are in, sorted. */
    val roomJids: List<String>,
) {
    val inHuddle: Boolean get() = huddleRoomJid != null
}

/** Who is in one joined room right now. */
data class RoomHeadcount(
    /** Every occupant except the account itself (bots and anonymous occupants count). */
    val here: Int,
    /** Present occupants advertising active Muji, the account included. */
    val inHuddle: Int,
)

/** The People projection at one instant. Immutable. */
data class PeopleDirectorySnapshot(
    /** Deduped by bare JID; self and bots excluded; by name, then JID. */
    val people: List<DirectoryPerson>,
    /** Normalised room JID → headcount; lookups by any case resolve. */
    val rooms: Map<String, RoomHeadcount>,
) {
    companion object {
        val EMPTY = PeopleDirectorySnapshot(people = emptyList(), rooms = BareJidMap.empty())
    }
}

/** Everything the projection reads, as of one instant. */
internal data class PeopleDirectoryInputs(
    /** [PresenceStore.occupants]: normalised room → nick → presence. */
    val occupants: Map<String, Map<String, WaddlePresence>>,
    /** [PresenceStore.contacts]: online contacts, collapsed. */
    val contacts: Map<String, ContactPresence>,
    /** Bot hats learned this session plus every room's declared bots. */
    val botJids: Set<String>,
    /** Muji participants: normalised room → nicks with an active call. */
    val huddleNicks: Map<String, Set<String>>,
    val ownBareJid: String?,
)

/**
 * The People projection, recomputed from whole inputs: one linear pass
 * over every occupant plus every contact (O(occupants + contacts)).
 *
 * Cost model: a reconnect floods one presence per occupant (30 rooms ×
 * 500 occupants is 15,000 presences). Recomputing per presence would be
 * quadratic, so [peopleDirectoryFlow] conflates inputs and recomputes
 * at most once per sample window; each recompute is linear.
 */
internal fun peopleDirectoryOf(inputs: PeopleDirectoryInputs): PeopleDirectorySnapshot {
    val own = inputs.ownBareJid?.let(::normalizedBareJid)
    val people = HashMap<String, PersonBuilder>()
    val rooms = HashMap<String, RoomHeadcount>()
    for ((room, occupants) in inputs.occupants) {
        rooms[room] = addRoom(room, occupants, inputs, own, people)
    }
    for ((bare, contact) in inputs.contacts) {
        // A room's own bare JID is not a person.
        val excluded = bare == own || bare in inputs.botJids || bare in inputs.occupants
        if (!excluded) people.getOrPut(bare) { PersonBuilder(bare) }.addContact(contact.show)
    }
    val sorted = people.values
        .map(PersonBuilder::build)
        .sortedWith(compareBy<DirectoryPerson>({ it.displayName.lowercase() }, { it.bareJid }))
    return PeopleDirectorySnapshot(people = sorted, rooms = BareJidMap(rooms))
}

/** One room's occupants into [people]; returns the room's headcount. */
private fun addRoom(
    room: String,
    occupants: Map<String, WaddlePresence>,
    inputs: PeopleDirectoryInputs,
    own: String?,
    people: MutableMap<String, PersonBuilder>,
): RoomHeadcount {
    val callNicks = inputs.huddleNicks[room].orEmpty()
    var here = 0
    var inHuddle = 0
    for ((nick, presence) in occupants) {
        // Real JID only from the occupant's CURRENT presence: the
        // nick → JID store keeps departed holders on purpose.
        val real = presence.mucJid?.let(::normalizedBareJid)?.takeIf { '@' in it }
        val isSelf = SELF_PRESENCE in presence.mucStatusCodes || (real != null && real == own)
        val inCall = nick in callNicks
        if (!isSelf) here++
        if (inCall) inHuddle++
        val excluded = isSelf || real == null || real in inputs.botJids || hasBotHat(presence.hats)
        if (!excluded) {
            people.getOrPut(real) { PersonBuilder(real) }
                .addOccupancy(room, nick, presenceShowOf(presence), inCall)
        }
    }
    return RoomHeadcount(here = here, inHuddle = inHuddle)
}

/**
 * The live projection: inputs are conflated and each snapshot is
 * followed by a [sampleMillis] pause, so a presence flood costs one
 * linear recompute per window, the first change shows at once and the
 * last change is never dropped. Nothing ticks while inputs are quiet.
 * The caller decides the dispatcher (off the main thread).
 */
internal fun peopleDirectoryFlow(
    presence: PresenceStore,
    roomBots: Flow<Map<String, RoomMembersState>>,
    huddleNicks: StateFlow<Map<String, Set<String>>>,
    sampleMillis: Long,
): Flow<PeopleDirectorySnapshot> =
    combine(
        presence.occupants,
        presence.contacts,
        combine(presence.botJids, roomBots) { learned, rooms -> botJidsOf(learned, rooms) },
        huddleNicks,
        presence.ownBareJid,
    ) { occupants, contacts, bots, nicks, own ->
        PeopleDirectoryInputs(occupants, contacts, bots, nicks, own)
    }
        .conflate()
        .transform { inputs ->
            emit(peopleDirectoryOf(inputs))
            delay(sampleMillis)
        }

private fun botJidsOf(learned: Set<String>, rooms: Map<String, RoomMembersState>): Set<String> {
    val declared: List<WaddleRoomBot> = rooms.values.flatMap { it.bots }
    if (declared.isEmpty()) return learned
    return learned + declared.map { normalizedBareJid(it.jid) }
}

/** Accumulates one person across rooms and contact presence. */
private class PersonBuilder(private val bareJid: String) {
    private var show = PresenceShow.OFFLINE
    private var nickRoom: String? = null
    private var nick: String? = null
    private var huddleRoom: String? = null
    private val rooms = ArrayList<String>(1)

    fun addOccupancy(room: String, occupantNick: String, occupantShow: PresenceShow, inCall: Boolean) {
        rooms += room
        if (occupantShow.outranks(show)) show = occupantShow
        if (isFirst(room, nickRoom)) {
            nickRoom = room
            nick = occupantNick
        }
        if (inCall && isFirst(room, huddleRoom)) huddleRoom = room
    }

    fun addContact(contactShow: PresenceShow) {
        if (contactShow.outranks(show)) show = contactShow
    }

    fun build() = DirectoryPerson(
        bareJid = bareJid,
        displayName = nick ?: bareJid.substringBefore('@'),
        show = show,
        huddleRoomJid = huddleRoom,
        // Two nicks of one account in one room list the room once.
        roomJids = rooms.distinct().sorted(),
    )

    private fun isFirst(room: String, current: String?): Boolean = current == null || room < current
}

/** XEP-0045 §7.2.2: the presence is about the recipient itself. */
private const val SELF_PRESENCE: UShort = 110u
