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
import social.waddle.android.client.calls.CallKind
import social.waddle.android.client.calls.CallState
import social.waddle.android.client.normalizedBareJid
import social.waddle.android.client.store.ContactPresence
import social.waddle.android.client.store.InboxEntrySnapshot
import social.waddle.android.client.store.NotifySettingsEntry
import social.waddle.android.client.store.PeopleDirectorySnapshot
import social.waddle.android.client.store.PresenceShow
import social.waddle.android.jid.localpartOf
import social.waddle.android.viewModelFactoryOf
import social.waddle.client.ffi.WaddleChannel
import social.waddle.client.ffi.WaddleNotifyMode

/** One row of the merged DM surface: a 1:1 peer or a group DM. */
sealed interface DmListRow {
    val unreadCount: Int
    val isMuted: Boolean

    /** XEP-0430 inbox last message preview, when the server sent one. */
    val preview: String?

    /** Inbox last-updated (epoch millis), for the row's time. */
    val lastUpdated: Long?

    data class Peer(
        val peerJid: String,
        val name: String,
        override val unreadCount: Int,
        /** XEP-0492: an explicit `never` override mutes this DM. */
        override val isMuted: Boolean = false,
        override val preview: String? = null,
        override val lastUpdated: Long? = null,
    ) : DmListRow

    data class Group(
        val roomJid: String,
        val name: String,
        override val unreadCount: Int,
        override val isMuted: Boolean = false,
        override val preview: String? = null,
        override val lastUpdated: Long? = null,
    ) : DmListRow
}

/** "author: text" for group previews, the bare text for 1:1. */
internal fun previewOf(entry: InboxEntrySnapshot?, withAuthor: Boolean): String? {
    val text = entry?.preview?.trim()?.takeIf { it.isNotEmpty() } ?: return null
    val author = entry.author?.let { localpartOf(it.substringAfter('/', it)) }
    return if (withAuthor && author != null) "$author: $text" else text
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

    /** [rows] with who is here and what is live: presence, calls, mentions. */
    val items: StateFlow<List<DmListItem>> = combine(
        rows,
        combine(sessionManager.peopleDirectory, sessionManager.presenceStore.contacts, ::Pair),
        sessionManager.callStore.state,
        sessionManager.callStore.mucCallPresence.participants,
        sessionManager.mentionStore.counts,
    ) { rows, (directory, contacts), call, huddles, mentions ->
        decoratedDmRows(
            rows,
            DmLiveInputs(
                directory = directory,
                contacts = contacts,
                call = call,
                huddleNicks = huddles,
                mentions = mentions,
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
            preview = previewOf(inputs.directInbox[peerJid], withAuthor = false),
            lastUpdated = inputs.directInbox[peerJid]?.lastUpdated,
        )
        row to inputs.directInbox[peerJid]?.lastUpdated
    }
    val groupRows = inputs.groupDms.map { channel ->
        val row = DmListRow.Group(
            roomJid = channel.roomJid,
            name = channel.name,
            unreadCount = inputs.counts[channel.roomJid] ?: 0,
            isMuted = mutedOf(channel.roomJid),
            preview = previewOf(inputs.mucInbox[channel.roomJid], withAuthor = true),
            lastUpdated = inputs.mucInbox[channel.roomJid]?.lastUpdated,
        )
        row to inputs.mucInbox[channel.roomJid]?.lastUpdated
    }
    val (timed, untimed) = (peerRows + groupRows).partition { (_, lastUpdated) -> lastUpdated != null }
    return timed.sortedByDescending { (_, lastUpdated) -> lastUpdated }.map { (row, _) -> row } +
        untimed.map { (row, _) -> row }
}

/** Whether a DM row has a call happening right now (ember when it does). */
enum class DmLiveState {
    NONE,

    /** A 1:1 call with this peer is ringing (theirs or ours). */
    RINGING,

    /** We are in a call with this peer, or the group DM has a huddle going. */
    LIVE,
}

/** One DM-list row as rendered: the merged row plus presence and liveness. */
data class DmListItem(
    val row: DmListRow,
    /** The peer's most-available Show; `null` when unknown (groups, or never seen). */
    val presence: PresenceShow? = null,
    /** The peer is in a huddle somewhere (teal ring). */
    val inHuddle: Boolean = false,
    val live: DmLiveState = DmLiveState.NONE,
    /** Group-DM mentions of the account; live-only, like the web. */
    val mentionCount: Int = 0,
    /** People in the group DM's huddle right now. */
    val huddleCount: Int = 0,
)

/** Everything liveness reads, bundled for the pure decoration. */
internal data class DmLiveInputs(
    val directory: PeopleDirectorySnapshot = PeopleDirectorySnapshot.EMPTY,
    val contacts: Map<String, ContactPresence> = emptyMap(),
    val call: CallState = CallState.Idle,
    /** Normalised room JID → nicks with an active Muji call. */
    val huddleNicks: Map<String, Set<String>> = emptyMap(),
    val mentions: Map<String, Int> = emptyMap(),
)

/**
 * Presence comes from the People projection (rooms and contacts,
 * most-available) and falls back to contact presence; a peer seen
 * nowhere this stream has no dot rather than a guessed "offline".
 */
internal fun decoratedDmRows(rows: List<DmListRow>, inputs: DmLiveInputs): List<DmListItem> {
    val people = inputs.directory.people.associateBy { it.bareJid }
    return rows.map { row ->
        when (row) {
            is DmListRow.Peer -> {
                val bare = normalizedBareJid(row.peerJid)
                val person = people[bare]
                DmListItem(
                    row = row,
                    presence = person?.show ?: inputs.contacts[bare]?.show,
                    inHuddle = person?.inHuddle == true,
                    live = peerLiveState(bare, inputs.call),
                    mentionCount = inputs.mentions[row.peerJid] ?: 0,
                )
            }
            is DmListRow.Group -> {
                val inCall = inputs.huddleNicks[normalizedBareJid(row.roomJid)].orEmpty().size
                DmListItem(
                    row = row,
                    live = if (inCall > 0) DmLiveState.LIVE else DmLiveState.NONE,
                    mentionCount = inputs.mentions[row.roomJid] ?: 0,
                    huddleCount = inCall,
                )
            }
        }
    }
}

private fun peerLiveState(bare: String, call: CallState): DmLiveState = when (call) {
    is CallState.Incoming -> if (normalizedBareJid(call.from) == bare) DmLiveState.RINGING else DmLiveState.NONE
    is CallState.Outgoing -> if (normalizedBareJid(call.to) == bare) DmLiveState.RINGING else DmLiveState.NONE
    is CallState.Active -> if (call.kind == CallKind.DM && normalizedBareJid(call.peer) == bare) {
        DmLiveState.LIVE
    } else {
        DmLiveState.NONE
    }
    else -> DmLiveState.NONE
}
