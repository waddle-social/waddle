package social.waddle.android.feature.home

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
import social.waddle.android.client.ConnectionState
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.jid.bareJidOf
import social.waddle.android.viewModelFactoryOf
import social.waddle.client.ffi.WaddleTopology

data class ChannelListItem(
    val roomJid: String,
    val name: String,
    val unreadCount: Int,
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
    val uiState: StateFlow<HomeUiState> = combine(
        sessionManager.roomStore.topology,
        sessionManager.unreadStore.counts,
        sessionManager.dmStore.peers,
        sessionManager.connectionState,
    ) { topology, counts, dmPeers, connection ->
        HomeUiState(
            sections = sectionsOf(topology, counts),
            dmUnreadCount = dmPeers.sumOf { counts[it] ?: 0 },
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

    private fun sectionsOf(
        topology: WaddleTopology,
        counts: Map<String, Int>,
    ): List<SpaceSection> {
        val channelsBySpace = topology.channels.groupBy { it.spaceId }
        val sections = topology.spaces.map { space ->
            SpaceSection(
                id = space.id,
                name = space.name,
                channels = channelsBySpace[space.id].orEmpty()
                    .sortedBy { it.position }
                    .map { channel ->
                        ChannelListItem(
                            roomJid = channel.roomJid,
                            name = channel.name,
                            unreadCount = counts[bareJidOf(channel.roomJid)] ?: 0,
                        )
                    },
            )
        }
        val knownSpaces = topology.spaces.mapTo(HashSet()) { it.id }
        val orphans = topology.channels
            .filter { it.spaceId !in knownSpaces }
            .sortedBy { it.position }
            .map { channel ->
                ChannelListItem(
                    roomJid = channel.roomJid,
                    name = channel.name,
                    unreadCount = counts[bareJidOf(channel.roomJid)] ?: 0,
                )
            }
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
