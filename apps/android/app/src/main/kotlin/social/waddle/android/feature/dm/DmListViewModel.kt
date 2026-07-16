package social.waddle.android.feature.dm

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import social.waddle.android.AppGraph
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.jid.localpartOf
import social.waddle.android.viewModelFactoryOf
import social.waddle.client.ffi.WaddleNotifyMode

data class DmListItem(
    val peerJid: String,
    val name: String,
    val unreadCount: Int,
    /** XEP-0492: an explicit `never` override mutes this DM. */
    val isMuted: Boolean = false,
)

/** Recent DM peers (most recently active first) with unread badges. */
class DmListViewModel(sessionManager: XmppSessionManager) : ViewModel() {
    val peers: StateFlow<List<DmListItem>> = combine(
        sessionManager.dmStore.peers,
        sessionManager.unreadStore.counts,
        sessionManager.notifySettingsStore.entries,
    ) { peers, counts, notifyEntries ->
        peers.map { peerJid ->
            DmListItem(
                peerJid = peerJid,
                name = localpartOf(peerJid),
                unreadCount = counts[peerJid] ?: 0,
                // The direct-chat §3 default is `always`, so only an
                // explicit stored override can mute.
                isMuted = notifyEntries[peerJid]?.notifyMode == WaddleNotifyMode.NEVER,
            )
        }
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), emptyList())

    companion object {
        fun factory(graph: AppGraph): ViewModelProvider.Factory =
            viewModelFactoryOf { DmListViewModel(graph.sessionManager) }
    }
}
