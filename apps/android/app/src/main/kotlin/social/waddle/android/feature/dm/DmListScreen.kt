package social.waddle.android.feature.dm

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.outlined.Group
import androidx.compose.material.icons.outlined.GroupAdd
import androidx.compose.material.icons.outlined.NotificationsOff
import androidx.compose.material.icons.outlined.Person
import androidx.compose.material3.Badge
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R

/** The merged DM surface: 1:1 peers and group DMs by inbox recency. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DmListScreen(
    onOpenDm: (peerJid: String, name: String) -> Unit,
    onOpenGroupDm: (roomJid: String, name: String) -> Unit,
    onBack: () -> Unit,
) {
    val graph = LocalAppGraph.current
    val viewModel: DmListViewModel = viewModel(factory = DmListViewModel.factory(graph))
    val rows by viewModel.rows.collectAsStateWithLifecycle()
    var newGroupOpen by remember { mutableStateOf(false) }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(text = stringResource(R.string.dm_list_title)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(
                            Icons.AutoMirrored.Filled.ArrowBack,
                            contentDescription = stringResource(R.string.action_back),
                        )
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
            Box(
                modifier = Modifier
                    .padding(padding)
                    .fillMaxSize()
                    .padding(32.dp),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    text = stringResource(R.string.dm_list_empty),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                )
            }
        } else {
            LazyColumn(modifier = Modifier.padding(padding)) {
                items(items = rows, key = ::dmRowKey) { row ->
                    when (row) {
                        is DmListRow.Peer -> DmSurfaceRow(
                            name = row.name,
                            subtitle = row.peerJid,
                            isGroup = false,
                            unreadCount = row.unreadCount,
                            isMuted = row.isMuted,
                            testTag = DmListTestTags.PEER_ROW_PREFIX + row.peerJid,
                            onClick = { onOpenDm(row.peerJid, row.name) },
                        )
                        is DmListRow.Group -> DmSurfaceRow(
                            name = row.name,
                            subtitle = stringResource(R.string.dm_list_group_subtitle),
                            isGroup = true,
                            unreadCount = row.unreadCount,
                            isMuted = row.isMuted,
                            testTag = DmListTestTags.GROUP_ROW_PREFIX + row.roomJid,
                            onClick = { onOpenGroupDm(row.roomJid, row.name) },
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

@Composable
private fun DmSurfaceRow(
    name: String,
    subtitle: String,
    isGroup: Boolean,
    unreadCount: Int,
    isMuted: Boolean,
    testTag: String,
    onClick: () -> Unit,
) {
    ListItem(
        headlineContent = { Text(text = name) },
        supportingContent = { Text(text = subtitle) },
        leadingContent = {
            Icon(
                if (isGroup) Icons.Outlined.Group else Icons.Outlined.Person,
                contentDescription = null,
            )
        },
        trailingContent = {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                if (isMuted) {
                    Icon(
                        Icons.Outlined.NotificationsOff,
                        contentDescription = stringResource(R.string.notify_muted_badge),
                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                if (unreadCount > 0) {
                    Badge { Text(text = unreadCount.toString()) }
                }
            }
        },
        modifier = Modifier
            .testTag(testTag)
            .clickable(onClick = onClick),
    )
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
