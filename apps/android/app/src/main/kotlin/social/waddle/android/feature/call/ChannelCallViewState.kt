package social.waddle.android.feature.call

import social.waddle.android.client.calls.CallKind
import social.waddle.android.client.calls.CallState

/** Whether the call slot is busy with a call OUTSIDE [roomJid]. */
private fun busyElsewhere(state: CallState, roomJid: String): Boolean {
    if (state is CallState.Idle || state is CallState.Ended) return false
    return !localCallInRoom(state, roomJid)
}

/** Whether this resource's call slot belongs to [roomJid]'s group call. */
fun localCallInRoom(state: CallState, roomJid: String): Boolean {
    val room = normalizeCallRoomJid(roomJid)
    if (room.isEmpty()) return false
    return when (state) {
        is CallState.MucPending -> state.roomJid == room
        is CallState.Active ->
            state.kind == CallKind.MUC && normalizeCallRoomJid(state.peer) == room
        else -> false
    }
}

/** What the channel top bar's call slot should render. */
sealed interface ChannelCallControls {
    /** No affordance: our slot is busy, or we're already in this room's call. */
    data object Hidden : ChannelCallControls

    /** No call live here: show the start-audio + start-video buttons. */
    data object Start : ChannelCallControls

    /** The room has a live call we're not in: show the join pill. */
    data class Join(val participantCount: Int, val video: Boolean) : ChannelCallControls
}

/**
 * Top-bar affordance for a channel (web MucCallButton semantics): the
 * start buttons vanish while any call occupies the single slot, and a
 * room-live call turns them into a single join affordance.
 */
fun channelCallControlsOf(
    state: CallState,
    roomJid: String,
    participants: List<String>,
    videoCall: Boolean,
): ChannelCallControls = when {
    localCallInRoom(state, roomJid) || busyElsewhere(state, roomJid) -> ChannelCallControls.Hidden
    participants.isNotEmpty() -> ChannelCallControls.Join(participants.size, videoCall)
    else -> ChannelCallControls.Start
}

/** What the above-timeline conversation banner should render. */
sealed interface ChannelCallBannerState {
    data object Hidden : ChannelCallBannerState

    /** A live call we're not in: "N in call" + nicks + Join. */
    data class Join(
        val participantCount: Int,
        val nicks: List<String>,
        val video: Boolean,
        /** The slot is busy with another call: join is disabled. */
        val busy: Boolean,
    ) : ChannelCallBannerState

    /** We're in this room's call (surface minimized): compact pill. */
    data class Ongoing(val participantCount: Int) : ChannelCallBannerState

    /**
     * This resource still owes the room's call a cleanup — a cached
     * session (Muji ghost or pending mixer terminate) with no live
     * local call: offer "Leave call" (web `leaveRetainedMucCallAction`
     * dock affordance).
     */
    data class LeaveRetained(val participantCount: Int) : ChannelCallBannerState
}

/**
 * The session cache's view of the room's retained entry, when one
 * exists for the current account (see `MucCallSessionCache.retainedEntry`).
 */
data class RetainedSessionView(
    /** The entry's XEP-0166 mixer terminate never made it to the wire. */
    val terminatePending: Boolean,
    /** The (possibly dead) full-JID resource that minted the entry. */
    val selfFullJid: String? = null,
)

/** Our own MUC identity: occupant nick plus the current bound full JID. */
data class SelfCallIdentity(
    val nick: String?,
    val fullJid: String?,
)

/**
 * One room's Muji presence evidence: the advertised nicks plus the
 * XEP-0045 owner (real full JID) the room exposes per nick.
 */
data class RoomMujiView(
    val nicks: List<String>,
    val owners: Map<String, String?>,
)

/**
 * Whether the retained-call cleanup affordance applies: our slot holds
 * NO call in this room, yet either a cached entry still owes the
 * mixer its terminate, or the Muji view advertises our nick under an
 * identity only a DEAD resource of ours explains (post-process-death
 * ghost — a cache entry is NOT required: a death before the mixer
 * accept caches nothing, and `muc.leaveRetained` handles the
 * presence-only clear). A nick owned by ANOTHER live resource of this
 * account, or by a foreign occupant sharing it, is real participation
 * — the caller offers Join, never the destructive leave.
 */
fun shouldOfferRetainedLeave(
    state: CallState,
    roomJid: String,
    retained: RetainedSessionView?,
    muji: RoomMujiView,
    self: SelfCallIdentity,
): Boolean {
    if (localCallInRoom(state, roomJid)) return false
    if (retained?.terminatePending == true) return true
    val nick = self.nick
    if (nick.isNullOrEmpty() || nick !in muji.nicks) return false
    return nickOwnerIsOurDeadResource(muji.owners[nick], self.fullJid, retained?.selfFullJid)
}

/**
 * Whether the advertised nick's XEP-0045 owner identifies OUR dead
 * resource: no exposed owner (the room hides real JIDs — the ghost
 * reading is the only evidence left), the cache entry's resource, or
 * our CURRENT resource (Android resource strings are stable across
 * process death, and with no live local call in the room its
 * advertisement is stale by definition). Any other full JID — a
 * sibling resource of ours, or a foreign bare JID — means live
 * participation under a shared nick.
 */
private fun nickOwnerIsOurDeadResource(
    owner: String?,
    selfFullJid: String?,
    cachedSelfFullJid: String?,
): Boolean {
    val ownerKey = fullJidIdentityKey(owner)
    if (ownerKey.isEmpty()) return true
    val cachedKey = fullJidIdentityKey(cachedSelfFullJid)
    if (cachedKey.isNotEmpty() && ownerKey == cachedKey) return true
    val selfKey = fullJidIdentityKey(selfFullJid)
    return selfKey.isNotEmpty() && ownerKey == selfKey
}

/**
 * Conversation-banner state (web ConversationCallBanner group branch):
 * live-call join surface while we're out, a compact ongoing pill while
 * our own call in this room is minimized behind the chat, and the
 * retained-call cleanup affordance when this resource still advertises
 * a call it is no longer in.
 */
fun channelCallBannerOf(
    state: CallState,
    roomJid: String,
    participants: List<String>,
    videoCall: Boolean,
    retainedLeave: Boolean = false,
): ChannelCallBannerState = when {
    localCallInRoom(state, roomJid) ->
        ChannelCallBannerState.Ongoing(participantCount = maxOf(participants.size, 1))
    retainedLeave -> ChannelCallBannerState.LeaveRetained(participantCount = maxOf(participants.size, 1))
    participants.isNotEmpty() -> ChannelCallBannerState.Join(
        participantCount = participants.size,
        nicks = participants,
        video = videoCall,
        busy = busyElsewhere(state, roomJid),
    )
    else -> ChannelCallBannerState.Hidden
}
