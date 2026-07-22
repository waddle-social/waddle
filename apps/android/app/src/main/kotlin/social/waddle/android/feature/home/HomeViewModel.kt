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
import social.waddle.android.client.store.NotifySettingsEntry
import social.waddle.android.jid.bareJidOf
import social.waddle.android.viewModelFactoryOf
import social.waddle.client.ffi.WaddleChannel
import social.waddle.client.ffi.WaddleNotifyMode
import social.waddle.client.ffi.WaddleSpace

data class ChannelListItem(
    val roomJid: String,
    val name: String,
    val unreadCount: Int,
    /** XEP-0492: an explicit `never` override mutes this channel. */
    val isMuted: Boolean = false,
    /** XEP-0272: occupants advertising the room's live group call. */
    val inCallCount: Int = 0,
)

/** One drawer/list section; `name == null` is the unspaced-channels bucket. */
data class SpaceSection(
    val id: String,
    val name: String?,
    val channels: List<ChannelListItem>,
)

data class HomeUiState(
    val sections: List<SpaceSection> = emptyList(),
    val dmUnreadCount: Int = 0,
    val connectionState: ConnectionState = ConnectionState.Idle,
)

/** Spaces→channels topology with unread badges and the connection banner. */
class HomeViewModel(
    private val sessionManager: XmppSessionManager,
    private val nick: String,
) : ViewModel() {
    /** Failed-banner action: restart the parked loop with a fresh budget. */
    fun retryConnection() {
        sessionManager.requestReconnect()
    }

    val uiState: StateFlow<HomeUiState> = combine(
        // Spaces come from the raw topology; the channel list uses the
        // store's group-DM-free selector (group DMs surface on the DM
        // list in stage 3, never here).
        combine(
            sessionManager.roomStore.topology,
            sessionManager.roomStore.channels,
            ::Pair,
        ),
        sessionManager.unreadStore.counts,
        sessionManager.dmStore.peers,
        sessionManager.connectionState,
        // XEP-0272 Muji view rides along with the bookmark entries so
        // the five-arity combine stays available.
        combine(
            sessionManager.notifySettingsStore.entries,
            sessionManager.callStore.mucCallPresence.participants,
            ::Pair,
        ),
    ) { (topology, channels), counts, dmPeers, connection, (notifyEntries, callParticipants) ->
        // Group DMs live on the DM surface, so their unreads count into
        // the drawer's Direct-messages badge alongside peer DMs.
        val groupDmUnread = topology.channels
            .filter { it.isGroupDm }
            .sumOf { counts[it.roomJid] ?: 0 }
        HomeUiState(
            sections = sectionsOf(topology.spaces, channels, counts, notifyEntries, callParticipants),
            dmUnreadCount = dmPeers.sumOf { counts[it] ?: 0 } + groupDmUnread,
            connectionState = connection,
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

    private fun sectionsOf(
        spaces: List<WaddleSpace>,
        channels: List<WaddleChannel>,
        counts: Map<String, Int>,
        notifyEntries: Map<String, NotifySettingsEntry>,
        callParticipants: Map<String, Set<String>>,
    ): List<SpaceSection> {
        fun itemOf(roomJid: String, name: String): ChannelListItem {
            val bare = bareJidOf(roomJid)
            return ChannelListItem(
                roomJid = roomJid,
                name = name,
                unreadCount = counts[bare] ?: 0,
                // A group's §3 default is never `never`, so only an
                // explicit stored override can mute it.
                isMuted = notifyEntries[bare]?.notifyMode == WaddleNotifyMode.NEVER,
                // Muji presence keys rooms by normalized (lowercased)
                // bare JID.
                inCallCount = callParticipants[bare.lowercase()]?.size ?: 0,
            )
        }

        val channelsBySpace = channels.groupBy { it.spaceId }
        val sections = spaces.map { space ->
            SpaceSection(
                id = space.id,
                name = space.name,
                channels = channelsBySpace[space.id].orEmpty()
                    .sortedBy { it.position }
                    .map { channel -> itemOf(channel.roomJid, channel.name) },
            )
        }
        val knownSpaces = spaces.mapTo(HashSet()) { it.id }
        val orphans = channels
            .filter { it.spaceId !in knownSpaces }
            .sortedBy { it.position }
            .map { channel -> itemOf(channel.roomJid, channel.name) }
        return if (orphans.isEmpty()) {
            sections
        } else {
            sections + SpaceSection(id = ORPHAN_SECTION_ID, name = null, channels = orphans)
        }
    }

    companion object {
        private const val ORPHAN_SECTION_ID = "waddle.section.unspaced"

        fun factory(graph: AppGraph): ViewModelProvider.Factory = viewModelFactoryOf {
            HomeViewModel(
                sessionManager = graph.sessionManager,
                nick = graph.currentSession.value?.xmppLocalpart ?: DEFAULT_NICK,
            )
        }
    }
}
