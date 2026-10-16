package social.waddle.android.feature.rooms

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import social.waddle.android.AppGraph
import social.waddle.android.DEFAULT_NICK
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.feature.call.MucCallLiveParticipantsStore
import social.waddle.android.viewModelFactoryOf

data class RoomsUiState(
    /** Every listable room, sorted by [RoomActivityOrder]. */
    val rooms: List<RoomActivity> = emptyList(),
    /** False until the topology had its chance to load. */
    val settled: Boolean = false,
)

/** The Rooms tab: every room, sorted by what is happening in it. */
class RoomsViewModel(
    private val sessionManager: XmppSessionManager,
    private val nick: String,
    liveParticipants: MucCallLiveParticipantsStore,
) : ViewModel() {
    val uiState: StateFlow<RoomsUiState> = combine(
        roomActivityFlow(sessionManager, liveParticipants),
        topologySettledFlow(sessionManager),
    ) { rooms, settled ->
        RoomsUiState(rooms = rooms.sortedWith(RoomActivityOrder), settled = settled)
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), RoomsUiState())

    /** Join (nick = account localpart) as the room opens; MAM backfills either way. */
    fun openRoom(roomJid: String) {
        viewModelScope.launch { sessionManager.joinRoom(roomJid, nick) }
    }

    companion object {
        fun factory(graph: AppGraph): ViewModelProvider.Factory = viewModelFactoryOf {
            RoomsViewModel(
                sessionManager = graph.sessionManager,
                nick = graph.currentSession.value?.xmppLocalpart ?: DEFAULT_NICK,
                liveParticipants = graph.mucCallLiveParticipants,
            )
        }
    }
}
