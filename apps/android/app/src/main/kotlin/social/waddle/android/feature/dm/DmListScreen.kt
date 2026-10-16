package social.waddle.android.feature.dm

import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.outlined.ChatBubbleOutline
import androidx.compose.material.icons.outlined.Group
import androidx.compose.material.icons.outlined.GroupAdd
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.ScaffoldDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.avatar.LIST_AVATAR_SIZE
import social.waddle.android.avatar.PeerAvatar
import social.waddle.android.ui.ConversationListRow
import social.waddle.android.ui.EmptyState
import social.waddle.android.ui.GlyphTile
import social.waddle.android.ui.WaddleTopBar
import social.waddle.android.ui.listTimeOf
import java.time.Instant

/**
 * The merged DM surface: 1:1 peers and group DMs by inbox recency. As a
 * tab it has no back affordance ([onBack] null).
 */
@Composable
fun DmListScreen(
    onOpenDm: (peerJid: String, name: String) -> Unit,
    onOpenGroupDm: (roomJid: String, name: String) -> Unit,
    onBack: (() -> Unit)? = null,
    contentWindowInsets: WindowInsets = ScaffoldDefaults.contentWindowInsets,
) {
    val graph = LocalAppGraph.current
    val viewModel: DmListViewModel = viewModel(factory = DmListViewModel.factory(graph))
    val rows by viewModel.rows.collectAsStateWithLifecycle()
    var newGroupOpen by remember { mutableStateOf(false) }

    Scaffold(
        contentWindowInsets = contentWindowInsets,
        topBar = {
            WaddleTopBar(
                title = { Text(text = stringResource(R.string.dm_list_title)) },
                navigationIcon = {
                    if (onBack != null) {
                        IconButton(onClick = onBack) {
                            Icon(
                                Icons.AutoMirrored.Filled.ArrowBack,
                                contentDescription = stringResource(R.string.action_back),
                            )
                        }
                    }
                },
                actions = {
                    IconButton(
                        onClick = { newGroupOpen = true },
                        modifier = Modifier.testTag(DmListTestTags.NEW_GROUP_ACTION),
                    ) {
                        Icon(
                            Icons.Outlined.GroupAdd,
                            contentDescription = stringResource(R.string.dm_list_new_group_action),
                        )
                    }
                },
            )
        },
    ) { padding ->
        if (rows.isEmpty()) {
            EmptyState(
                icon = Icons.Outlined.ChatBubbleOutline,
                title = stringResource(R.string.dm_list_empty_title),
                body = stringResource(R.string.dm_list_empty),
                modifier = Modifier.padding(padding),
            )
        } else {
            LazyColumn(modifier = Modifier.padding(padding).consumeWindowInsets(padding)) {
                items(items = rows, key = ::dmRowKey, contentType = { "dm" }) { row ->
                    when (row) {
                        is DmListRow.Peer -> ConversationListRow(
                            title = row.name,
                            leading = {
                                PeerAvatar(jid = row.peerJid, displayName = row.name, size = LIST_AVATAR_SIZE)
                            },
                            onClick = { onOpenDm(row.peerJid, row.name) },
                            preview = row.preview ?: row.peerJid,
                            time = row.lastUpdated?.let { listTimeOf(Instant.ofEpochMilli(it)) },
                            unreadCount = row.unreadCount,
                            isMuted = row.isMuted,
                            modifier = Modifier.testTag(DmListTestTags.PEER_ROW_PREFIX + row.peerJid),
                        )
                        is DmListRow.Group -> ConversationListRow(
                            title = row.name,
                            leading = { GlyphTile(Icons.Outlined.Group) },
                            onClick = { onOpenGroupDm(row.roomJid, row.name) },
                            preview = row.preview ?: stringResource(R.string.dm_list_group_subtitle),
                            time = row.lastUpdated?.let { listTimeOf(Instant.ofEpochMilli(it)) },
                            unreadCount = row.unreadCount,
                            isMuted = row.isMuted,
                            modifier = Modifier.testTag(DmListTestTags.GROUP_ROW_PREFIX + row.roomJid),
                        )
                    }
                }
            }
        }
    }

    if (newGroupOpen) {
        NewGroupDmSheet(
            onDismiss = { newGroupOpen = false },
            onCreated = { roomJid, name ->
                newGroupOpen = false
                onOpenGroupDm(roomJid, name)
            },
        )
    }
}

private fun dmRowKey(row: DmListRow): String = when (row) {
    is DmListRow.Peer -> "peer:${row.peerJid}"
    is DmListRow.Group -> "group:${row.roomJid}"
}

/** Semantics tags shared with instrumented tests. */
object DmListTestTags {
    const val NEW_GROUP_ACTION = "dm-list-new-group-action"
    const val PEER_ROW_PREFIX = "dm-list-peer:"
    const val GROUP_ROW_PREFIX = "dm-list-group:"
}
