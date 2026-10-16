package social.waddle.android.feature.rooms

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.transformLatest
import social.waddle.android.client.ConnectionState
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.feature.call.MucCallLiveParticipantsStore

/** The live room projection behind Home and Rooms (see [roomActivitiesOf]). */
fun roomActivityFlow(
    sessionManager: XmppSessionManager,
    liveParticipants: MucCallLiveParticipantsStore,
): Flow<List<RoomActivity>> {
    val muji = sessionManager.callStore.mucCallPresence
    val calls = combine(
        combine(muji.participants, muji.owners, muji.media, ::Triple),
        liveParticipants.participants,
        liveParticipants.leavingRooms,
        sessionManager.callStore.state,
    ) { (participants, owners, media), live, leaving, state ->
        RoomCallInputs(participants, owners, media, live, leaving, state)
    }
    val counts = combine(
        sessionManager.mentionStore.counts,
        sessionManager.unreadStore.counts,
        sessionManager.inboxStore.threadUnread,
        ::Triple,
    )
    val rooms = combine(
        sessionManager.roomStore.channels,
        sessionManager.roomStore.joinedRooms,
        sessionManager.peopleDirectory.map { it.rooms }.distinctUntilChanged(),
        ::Triple,
    )
    return combine(
        rooms,
        counts,
        sessionManager.inboxStore.muc,
        sessionManager.notifySettingsStore.entries,
        calls,
    ) { (channels, joined, headcounts), (mentions, unread, threadUnread), inbox, notify, callInputs ->
        roomActivitiesOf(
            RoomInputs(
                channels = channels,
                joinedRooms = joined,
                headcounts = headcounts,
                mentions = mentions,
                unread = unread,
                threadUnread = threadUnread,
                inbox = inbox,
                notify = notify,
                calls = callInputs,
            ),
        )
    }
}

/**
 * Whether the topology has had its chance to load: true once channels
 * exist, or once the stream has been ready for [graceMillis] (the ready
 * pipeline fetches the topology right after `Ready`, and an empty
 * community answers with no channels). Until then lists show skeletons,
 * never an empty state that would claim there is nothing.
 */
@OptIn(ExperimentalCoroutinesApi::class)
fun topologySettledFlow(
    sessionManager: XmppSessionManager,
    graceMillis: Long = TOPOLOGY_GRACE_MILLIS,
): Flow<Boolean> {
    val readyLongEnough = sessionManager.connectionState
        .map { it is ConnectionState.Ready }
        .distinctUntilChanged()
        .transformLatest { ready ->
            emit(false)
            if (ready) {
                delay(graceMillis)
                emit(true)
            }
        }
    return combine(sessionManager.roomStore.topology, readyLongEnough) { topology, ready ->
        topology.channels.isNotEmpty() || ready
    }.distinctUntilChanged()
}

/** Long enough for the topology IQ on a slow link; short enough not to feel stuck. */
const val TOPOLOGY_GRACE_MILLIS = 4_000L
