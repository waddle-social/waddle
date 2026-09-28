package social.waddle.android.feature.call

import social.waddle.android.client.normalizedBareJid
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
 * One live (LiveKit) participant: its row label and avatar JID both come
 * from the SAME identity record. [ownedNick] marks a label taken from a
 * Muji owner entry that matched this exact identity (a real room nick);
 * otherwise the label is the identity's localpart.
 */
private data class LiveParticipant(val nick: String, val jid: String?, val ownedNick: Boolean)

/**
 * Map LiveKit identities (full JIDs) to roster participants via the Muji
 * owner map (nick → real JID), used only on an exact identity match —
 * or for another device of the same account whose identity did match.
 * Identities without one degrade to their JID localpart until the next
 * presence render resolves them (web `identitiesToNicks`). One person on
 * two sessions (same label and JID) collapses to one participant.
 */
private fun liveParticipantsOf(
    identities: List<String>,
    owners: Map<String, String?>,
): List<LiveParticipant> {
    val ownerNickByIdentity = HashMap<String, String>()
    for ((nick, realJid) in owners) {
        val key = fullJidIdentityKey(realJid)
        if (key.isNotEmpty()) ownerNickByIdentity[key] = nick
    }
    // Another device of an owner-matched LIVE identity is the same
    // account: it takes that owner's label instead of its localpart.
    val ownerNickByBareJid = HashMap<String, String>()
    for (identity in identities) {
        val nick = ownerNickByIdentity[fullJidIdentityKey(identity)] ?: continue
        ownerNickByBareJid.putIfAbsent(normalizedBareJid(identity), nick)
    }
    val out = LinkedHashMap<Pair<String, String?>, LiveParticipant>()
    for (identity in identities) {
        val key = fullJidIdentityKey(identity)
        if (key.isEmpty()) continue
        val ownerNick = ownerNickByIdentity[key] ?: ownerNickByBareJid[normalizedBareJid(identity)]
        val participant = LiveParticipant(
            nick = ownerNick ?: localpartOf(identity),
            jid = normalizedBareJid(identity).takeIf { '@' in it },
            ownedNick = ownerNick != null,
        )
        // One person on two devices collapses to one row; whichever
        // identity arrives first, the row keeps the owner-matched flag.
        out.merge(participant.nick to participant.jid, participant) { first, second ->
            if (first.ownedNick) first else second
        }
    }
    return out.values.toList()
}

private fun identitiesToNicks(identities: List<String>, owners: Map<String, String?>): List<String> =
    liveParticipantsOf(identities, owners).map { it.nick }.distinct()

/** One roster row of the in-call MUC surface. */
data class MucRosterEntry(
    val nick: String,
    /** `urn:waddle:in-call:0` raised-hand marker from Muji presence. */
    val handRaised: Boolean,
    /** `urn:waddle:in-call:0` self-reported mute marker. */
    val muted: Boolean,
    /** The participant's real bare JID (avatar); `null` = unknown. */
    val jid: String? = null,
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

/**
 * Resolve the roster rows plus presence badges for [roomJid]. Live rows
 * take label and avatar JID from one identity record (no label-keyed
 * side map that another nick could overwrite); Muji-only rows take the
 * JID from their own owner entry.
 */
fun mucRosterOf(
    roomJid: String?,
    presence: MucPresenceRosterView,
    live: LiveRosterView,
): List<MucRosterEntry> {
    val room = roomJid?.let(::normalizeCallRoomJid).orEmpty()
    if (room.isEmpty()) return emptyList()
    val raised = presence.raisedHands[room].orEmpty()
    val muted = presence.mutedNicks[room].orEmpty()
    val owners = presence.owners[room].orEmpty()
    val liveIdentities = live.participants[room].orEmpty()
    if (liveIdentities.isNotEmpty()) {
        return liveParticipantsOf(liveIdentities, owners).map { participant ->
            // Badges are keyed by room nick: only an owner-matched label is one.
            MucRosterEntry(
                nick = participant.nick,
                handRaised = participant.ownedNick && participant.nick in raised,
                muted = participant.ownedNick && participant.nick in muted,
                jid = participant.jid,
            )
        }
    }
    val nicks = resolveRoomParticipantList(
        room, presence.participants, presence.owners, live.participants, live.leavingRooms,
    )
    return nicks.map { nick ->
        MucRosterEntry(
            nick = nick,
            handRaised = nick in raised,
            muted = nick in muted,
            jid = owners[nick]?.let(::normalizedBareJid)?.takeIf { '@' in it },
        )
    }
}
