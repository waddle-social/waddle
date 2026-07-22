package social.waddle.android.feature.call

import social.waddle.android.jid.localpartOf

/** `room@muc.host/nick` → `room@muc.host` normalized for map keys. */
fun normalizeCallRoomJid(roomJid: String): String =
    roomJid.substringBefore('/').trim().lowercase()

/**
 * Case-normalized full-JID identity (web `fullJidIdentityKey`): bare
 * part lowercased, resource kept case-sensitive per RFC 7622.
 */
fun fullJidIdentityKey(fullJid: String?): String {
    val trimmed = fullJid?.trim().orEmpty()
    if (trimmed.isEmpty()) return ""
    val separator = trimmed.indexOf('/')
    if (separator < 0) return trimmed.lowercase()
    return trimmed.substring(0, separator).lowercase() + "/" + trimmed.substring(separator + 1)
}

/**
 * Resolve the nick list for a room's group call (web
 * `resolveRoomParticipantList`): prefer the LiveKit live projection
 * when populated — identities mapped to nicks via the Muji owners map,
 * falling back to the JID localpart until presence catches up — else
 * the Muji presence view, with the room's leave marker suppressing our
 * own stale nick so a local leave never bounces 0 → N → 0.
 */
fun resolveRoomParticipantList(
    roomJid: String?,
    participantsByRoom: Map<String, Set<String>>,
    ownersByRoom: Map<String, Map<String, String?>>,
    liveParticipantsByRoom: Map<String, List<String>>,
    leavingByRoom: Map<String, String> = emptyMap(),
): List<String> {
    val room = roomJid?.let(::normalizeCallRoomJid).orEmpty()
    if (room.isEmpty()) return emptyList()
    val liveIdentities = liveParticipantsByRoom[room].orEmpty()
    if (liveIdentities.isNotEmpty()) {
        return identitiesToNicks(liveIdentities, ownersByRoom[room].orEmpty())
    }
    val mujiNicks = participantsByRoom[room].orEmpty().toList()
    val leavingNick = leavingByRoom[room] ?: return mujiNicks
    // Once the Muji view drops the nick on its own the marker is a
    // pure no-op here; it is consumed separately so it can't mask a
    // genuine re-join.
    return mujiNicks.filter { nick -> nick != leavingNick }
}

/**
 * Map LiveKit identities (full JIDs) to MUC nicks via the Muji owner
 * map (nick → real JID), deduplicating identical nicks (one person on
 * two sessions). Identities without an owner mapping degrade to the
 * JID localpart until the next presence render resolves them (web
 * `identitiesToNicks`).
 */
private fun identitiesToNicks(
    identities: List<String>,
    owners: Map<String, String?>,
): List<String> {
    val byRealJid = HashMap<String, String>()
    for ((nick, realJid) in owners) {
        val key = fullJidIdentityKey(realJid)
        if (key.isNotEmpty()) byRealJid[key] = nick
    }
    val out = LinkedHashSet<String>()
    for (identity in identities) {
        val key = fullJidIdentityKey(identity)
        if (key.isEmpty()) continue
        out += byRealJid[key] ?: localpartOf(identity)
    }
    return out.toList()
}

/** One roster row of the in-call MUC surface. */
data class MucRosterEntry(
    val nick: String,
    /** `urn:waddle:in-call:0` raised-hand marker from Muji presence. */
    val handRaised: Boolean,
    /** `urn:waddle:in-call:0` self-reported mute marker. */
    val muted: Boolean,
)

/** One consistent read of the Muji-presence flows, keyed by room. */
data class MucPresenceRosterView(
    val participants: Map<String, Set<String>>,
    val owners: Map<String, Map<String, String?>>,
    val raisedHands: Map<String, Set<String>>,
    val mutedNicks: Map<String, Set<String>>,
)

/** One consistent read of the LiveKit projection store, keyed by room. */
data class LiveRosterView(
    val participants: Map<String, List<String>>,
    val leavingRooms: Map<String, String>,
)

/** Resolve the roster rows plus per-nick presence badges for [roomJid]. */
fun mucRosterOf(
    roomJid: String?,
    presence: MucPresenceRosterView,
    live: LiveRosterView,
): List<MucRosterEntry> {
    val room = roomJid?.let(::normalizeCallRoomJid).orEmpty()
    if (room.isEmpty()) return emptyList()
    val nicks = resolveRoomParticipantList(
        room, presence.participants, presence.owners, live.participants, live.leavingRooms,
    )
    val raised = presence.raisedHands[room].orEmpty()
    val muted = presence.mutedNicks[room].orEmpty()
    return nicks.map { nick ->
        MucRosterEntry(nick = nick, handRaised = nick in raised, muted = nick in muted)
    }
}
