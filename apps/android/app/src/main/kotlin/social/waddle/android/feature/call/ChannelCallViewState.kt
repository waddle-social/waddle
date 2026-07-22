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
}

/**
 * Conversation-banner state (web ConversationCallBanner group branch):
 * live-call join surface while we're out, a compact ongoing pill while
 * our own call in this room is minimized behind the chat.
 */
fun channelCallBannerOf(
    state: CallState,
    roomJid: String,
    participants: List<String>,
    videoCall: Boolean,
): ChannelCallBannerState = when {
    localCallInRoom(state, roomJid) ->
        ChannelCallBannerState.Ongoing(participantCount = maxOf(participants.size, 1))
    participants.isNotEmpty() -> ChannelCallBannerState.Join(
        participantCount = participants.size,
        nicks = participants,
        video = videoCall,
        busy = busyElsewhere(state, roomJid),
    )
    else -> ChannelCallBannerState.Hidden
}
