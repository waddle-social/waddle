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
import social.waddle.android.client.store.InboxEntrySnapshot
import social.waddle.android.client.store.NotifySettingsEntry
import social.waddle.android.jid.localpartOf
import social.waddle.android.viewModelFactoryOf
import social.waddle.client.ffi.WaddleChannel
import social.waddle.client.ffi.WaddleNotifyMode

/** One row of the merged DM surface: a 1:1 peer or a group DM. */
sealed interface DmListRow {
    data class Peer(
        val peerJid: String,
        val name: String,
        val unreadCount: Int,
        /** XEP-0492: an explicit `never` override mutes this DM. */
        val isMuted: Boolean = false,
    ) : DmListRow

    data class Group(
        val roomJid: String,
        val name: String,
        val unreadCount: Int,
        val isMuted: Boolean = false,
    ) : DmListRow
}

/**
 * The merged DM surface: recent 1:1 peers plus bookmarked group DMs,
 * ordered by XEP-0430 inbox recency. Rows without an inbox entry sort
 * after the timestamped ones — peers in [social.waddle.android.client.store.DmStore]
 * recency order (pre-inbox seed), group DMs in bookmark order.
 */
class DmListViewModel(sessionManager: XmppSessionManager) : ViewModel() {
    val rows: StateFlow<List<DmListRow>> = combine(
        combine(sessionManager.dmStore.peers, sessionManager.roomStore.groupDms, ::Pair),
        combine(sessionManager.inboxStore.direct, sessionManager.inboxStore.muc, ::Pair),
        sessionManager.unreadStore.counts,
        sessionManager.notifySettingsStore.entries,
    ) { (peers, groupDms), (directInbox, mucInbox), counts, notifyEntries ->
        mergedDmRows(
            DmSurfaceInputs(
                peers = peers,
                groupDms = groupDms,
                directInbox = directInbox,
                mucInbox = mucInbox,
                counts = counts,
                notifyEntries = notifyEntries,
            ),
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), emptyList())

    companion object {
        fun factory(graph: AppGraph): ViewModelProvider.Factory =
            viewModelFactoryOf { DmListViewModel(graph.sessionManager) }
    }
}

/** Everything the DM-surface merge reads, bundled for the pure merge. */
internal data class DmSurfaceInputs(
    val peers: List<String>,
    val groupDms: List<WaddleChannel>,
    val directInbox: Map<String, InboxEntrySnapshot>,
    val mucInbox: Map<String, InboxEntrySnapshot>,
    val counts: Map<String, Int>,
    val notifyEntries: Map<String, NotifySettingsEntry>,
)

/** Pure merge of the DM surface rows; extracted for direct testing. */
internal fun mergedDmRows(inputs: DmSurfaceInputs): List<DmListRow> {
    fun mutedOf(jid: String): Boolean =
        // The direct-chat/private-group default is `always`, so only
        // an explicit stored override can mute.
        inputs.notifyEntries[jid]?.notifyMode == WaddleNotifyMode.NEVER

    val peerRows = inputs.peers.map { peerJid ->
        val row = DmListRow.Peer(
            peerJid = peerJid,
            name = localpartOf(peerJid),
            unreadCount = inputs.counts[peerJid] ?: 0,
            isMuted = mutedOf(peerJid),
        )
        row to inputs.directInbox[peerJid]?.lastUpdated
    }
    val groupRows = inputs.groupDms.map { channel ->
        val row = DmListRow.Group(
            roomJid = channel.roomJid,
            name = channel.name,
            unreadCount = inputs.counts[channel.roomJid] ?: 0,
            isMuted = mutedOf(channel.roomJid),
        )
        row to inputs.mucInbox[channel.roomJid]?.lastUpdated
    }
    val (timed, untimed) = (peerRows + groupRows).partition { (_, lastUpdated) -> lastUpdated != null }
    return timed.sortedByDescending { (_, lastUpdated) -> lastUpdated }.map { (row, _) -> row } +
        untimed.map { (row, _) -> row }
}
