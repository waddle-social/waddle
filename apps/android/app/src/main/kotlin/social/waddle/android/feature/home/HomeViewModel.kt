package social.waddle.android.feature.home

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import social.waddle.android.AppGraph
import social.waddle.android.DEFAULT_NICK
import social.waddle.android.client.ConnectionState
import social.waddle.android.client.CreateRoomResult
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.store.DirectoryPerson
import social.waddle.android.feature.call.MucCallLiveParticipantsStore
import social.waddle.android.feature.rooms.RoomActivity
import social.waddle.android.feature.rooms.roomActivityFlow
import social.waddle.android.feature.rooms.topologySettledFlow
import social.waddle.android.viewModelFactoryOf
import social.waddle.client.ffi.WaddleChannel

data class HomeUiState(
    /** Every listable room, topology order (callers sort). */
    val rooms: List<RoomActivity> = emptyList(),
    /** The People directory: no self, no bots, by name. */
    val people: List<DirectoryPerson> = emptyList(),
    /** Unread direct and group-DM messages: the DMs tab badge. */
    val dmUnreadCount: Int = 0,
    val connectionState: ConnectionState = ConnectionState.Idle,
    /** False until the topology had its chance to load: show skeletons, not "nothing". */
    val settled: Boolean = false,
) {
    /** Something is live in a room: the ember dot on the Rooms tab. */
    val anyHuddleLive: Boolean get() = rooms.any { it.live }
}

/**
 * The shell's community state: every room with what is happening in it,
 * the people around, the DM badge and the connection. Home reads all of
 * it; the tab bar reads the badges. Also owns room creation (Home's "+"
 * and Rooms' "New room").
 */
class HomeViewModel(
    private val sessionManager: XmppSessionManager,
    private val nick: String,
    liveParticipants: MucCallLiveParticipantsStore = MucCallLiveParticipantsStore(),
) : ViewModel() {
    /** Failed-banner action: restart the parked loop with a fresh budget. */
    fun retryConnection() {
        sessionManager.requestReconnect()
    }

    val uiState: StateFlow<HomeUiState> = combine(
        roomActivityFlow(sessionManager, liveParticipants),
        sessionManager.peopleDirectory,
        combine(
            sessionManager.dmStore.peers,
            sessionManager.roomStore.groupDms,
            sessionManager.unreadStore.counts,
            ::dmUnreadOf,
        ),
        sessionManager.connectionState,
        topologySettledFlow(sessionManager),
    ) { rooms, directory, dmUnread, connection, settled ->
        HomeUiState(
            rooms = rooms,
            people = directory.people,
            dmUnreadCount = dmUnread,
            connectionState = connection,
            settled = settled,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), HomeUiState())

    /**
     * Join fires here (nick = account localpart); navigation happens
     * immediately, the timeline backfills via MAM regardless.
     */
    fun openChannel(roomJid: String) {
        viewModelScope.launch { sessionManager.joinRoom(roomJid, nick) }
    }

    private val _canCreateChannel = MutableStateFlow(false)

    /**
     * Web `canManageChannels` (owner-only) reduced to the best-effort
     * community-owner probe: Android has no topology roles yet, so a
     * space-owner-but-not-community-owner sees no create affordance
     * (documented parity gap). UI gating only — room creation is
     * re-authorized server-side.
     */
    val canCreateChannel: StateFlow<Boolean> = _canCreateChannel.asStateFlow()

    private val _createEvents = MutableSharedFlow<CreateRoomResult>(extraBufferCapacity = 4)

    /** Outcome of the last create-channel submit. */
    val createEvents: SharedFlow<CreateRoomResult> = _createEvents

    init {
        viewModelScope.launch {
            // Probe on every Ready transition until it succeeds: an
            // early probe would race the connect and stick false, and
            // a transient IQ failure retries on the next reconnect.
            sessionManager.connectionState.collect { state ->
                if (state is ConnectionState.Ready && !_canCreateChannel.value) {
                    _canCreateChannel.value = sessionManager.isCommunityOwner()
                }
            }
        }
    }

    /**
     * XEP-0045 §10.1 create + configure (web `muc` intent parity):
     * naive name slug as localpart, initial config carries
     * name/description/forum. The result event carries the room JID
     * so the screen can navigate into the new channel.
     */
    fun createChannel(name: String, description: String, forum: Boolean) {
        viewModelScope.launch {
            val result = sessionManager.createRoom(
                name = name,
                nick = nick,
                description = description.trim().takeIf { it.isNotEmpty() },
                forum = forum,
            )
            _createEvents.tryEmit(result)
        }
    }

    companion object {
        fun factory(graph: AppGraph): ViewModelProvider.Factory = viewModelFactoryOf {
            HomeViewModel(
                sessionManager = graph.sessionManager,
                nick = graph.currentSession.value?.xmppLocalpart ?: DEFAULT_NICK,
                liveParticipants = graph.mucCallLiveParticipants,
            )
        }
    }
}

/** Peer DMs plus group DMs (which live on the DM surface): the DMs tab badge. */
internal fun dmUnreadOf(peers: List<String>, groupDms: List<WaddleChannel>, counts: Map<String, Int>): Int =
    peers.sumOf { counts[it] ?: 0 } + groupDms.sumOf { counts[it.roomJid] ?: 0 }
