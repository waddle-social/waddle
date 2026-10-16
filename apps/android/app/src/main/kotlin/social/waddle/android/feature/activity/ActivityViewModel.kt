package social.waddle.android.feature.activity

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
import social.waddle.android.jid.localpartOf
import social.waddle.android.viewModelFactoryOf
import social.waddle.client.ffi.WaddleChannel

/** One unread conversation on the Activity tab. */
data class ActivityRow(
    val jid: String,
    val name: String,
    val kind: Kind,
    val unreadCount: Int,
    val preview: String?,
    val lastUpdated: Long?,
) {
    enum class Kind { CHANNEL, GROUP_DM, DIRECT }
}

/**
 * The unread overview (web `UnreadView`, iOS Activity): every
 * conversation with unread messages, newest activity first, with the
 * server inbox's last-message preview.
 */
class ActivityViewModel(sessionManager: XmppSessionManager) : ViewModel() {
    val rows: StateFlow<List<ActivityRow>> = combine(
        sessionManager.unreadStore.counts,
        sessionManager.inboxStore.direct,
        sessionManager.inboxStore.muc,
        sessionManager.roomStore.topology,
    ) { counts, direct, muc, topology ->
        activityRowsOf(counts, direct, muc, topology.channels)
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), emptyList())

    companion object {
        fun factory(graph: AppGraph): ViewModelProvider.Factory =
            viewModelFactoryOf { ActivityViewModel(graph.sessionManager) }
    }
}

/** Pure merge, extracted for direct testing. */
internal fun activityRowsOf(
    counts: Map<String, Int>,
    direct: Map<String, InboxEntrySnapshot>,
    muc: Map<String, InboxEntrySnapshot>,
    channels: List<WaddleChannel>,
): List<ActivityRow> {
    val byRoom = channels.associateBy { it.roomJid }
    return counts.filterValues { it > 0 }.map { (jid, unread) ->
        val channel = byRoom[jid]
        val entry = muc[jid] ?: direct[jid]
        val kind = when {
            channel?.isGroupDm == true -> ActivityRow.Kind.GROUP_DM
            channel != null || muc.containsKey(jid) -> ActivityRow.Kind.CHANNEL
            else -> ActivityRow.Kind.DIRECT
        }
        ActivityRow(
            jid = jid,
            name = channel?.name ?: localpartOf(jid),
            kind = kind,
            unreadCount = unread,
            preview = entry?.preview?.trim()?.takeIf { it.isNotEmpty() }?.let { text ->
                val author = entry.author?.substringAfter('/', entry.author.orEmpty())?.let(::localpartOf)
                if (kind != ActivityRow.Kind.DIRECT && author != null) "$author: $text" else text
            },
            lastUpdated = entry?.lastUpdated,
        )
    }.sortedWith(compareByDescending<ActivityRow> { it.lastUpdated ?: Long.MIN_VALUE }.thenBy { it.name })
}
