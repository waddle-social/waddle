package social.waddle.android.flutter

import social.waddle.android.AppGraph
import social.waddle.android.feature.call.ChannelCallControls
import social.waddle.android.feature.call.RetainedSessionView
import social.waddle.android.feature.call.RoomMujiView
import social.waddle.android.feature.call.SelfCallIdentity
import social.waddle.android.feature.call.channelCallControlsOf
import social.waddle.android.feature.call.localCallInRoom
import social.waddle.android.feature.call.normalizeCallRoomJid
import social.waddle.android.feature.call.resolveRoomParticipantList
import social.waddle.android.feature.call.shouldOfferRetainedLeave

/**
 * Read-only projection of the native channel call affordances, including process-death recovery.
 */
internal class FlutterSurfaceRoomCallState(private val graph: AppGraph) {
    suspend fun snapshot(jid: String): Map<String, Any?> {
        val room = normalizeCallRoomJid(jid)
        val manager = graph.sessionManager
        val session = graph.currentSession.value ?: error("Sign in first")
        val fullJid = manager.ownFullJid()
        val retained =
            graph.sessionPrefs.mucCallSessions.retainedEntry(room, fullJid)?.let {
                RetainedSessionView(
                    terminatePending = it.terminatePending,
                    selfFullJid = it.selfFullJid,
                )
            }
        check(graph.currentSession.value === session) { "Account changed while reading call state" }
        val presence = manager.callStore.mucCallPresence
        val state = manager.callStore.state.value
        val participants =
            resolveRoomParticipantList(
                room,
                presence.participants.value,
                presence.owners.value,
                graph.mucCallLiveParticipants.participants.value,
                graph.mucCallLiveParticipants.leavingRooms.value,
            )
        val video = presence.media.value[room]?.video == true
        val localCall = localCallInRoom(state, room)
        val controls = channelCallControlsOf(state, room, participants, video)
        val canLeave =
            shouldOfferRetainedLeave(
                state,
                room,
                retained,
                RoomMujiView(
                    presence.participants.value[room]?.toList().orEmpty(),
                    presence.owners.value[room].orEmpty(),
                ),
                SelfCallIdentity(
                    session.xmppLocalpart,
                    fullJid,
                    leaveEchoPending =
                        "$room/${session.xmppLocalpart}" in presence.selfLeaveEchoPending.value,
                ),
            )
        return mapOf(
            "jid" to room,
            "canLeaveRetained" to canLeave,
            "participants" to participants,
            "participantCount" to participants.size,
            "video" to video,
            "localCall" to localCall,
            "busy" to (controls == ChannelCallControls.Hidden && !localCall),
            "controls" to
                when (controls) {
                    ChannelCallControls.Hidden -> "hidden"
                    ChannelCallControls.Start -> "start"
                    is ChannelCallControls.Join -> "join"
                },
        )
    }
}
