package social.waddle.android.feature.members

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.outlined.AccountCircle
import androidx.compose.material.icons.outlined.PersonAdd
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.client.RoomAdminResult
import social.waddle.android.client.store.MemberListStatus
import social.waddle.client.ffi.WaddleMucAffiliation

/** A pending destructive member action awaiting confirmation. */
private sealed interface PendingAction {
    val row: MemberRow

    data class Kick(override val row: MemberRow) : PendingAction

    data class Ban(override val row: MemberRow) : PendingAction

    data class Remove(override val row: MemberRow) : PendingAction
}

/**
 * Member management screen (web `MemberManagement.vue` parity):
 * affiliation-sorted member list merged with live occupancy, XEP-0317
 * hats, and — for owners/admins — promote/demote/remove/ban/kick plus
 * the XEP-0055 add-member search. Owner rows are immutable.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MembersScreen(roomJid: String, name: String, onBack: () -> Unit) {
    val graph = LocalAppGraph.current
    val viewModel: MembersViewModel = viewModel(
        key = "members:$roomJid",
        factory = MembersViewModel.factory(graph, roomJid),
    )
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    var actionTarget by remember { mutableStateOf<MemberRow?>(null) }
    var pendingAction by remember { mutableStateOf<PendingAction?>(null) }
    val snackbarHostState = remember { SnackbarHostState() }
    val failedText = stringResource(R.string.action_failed)
    val failedOfflineText = stringResource(R.string.action_failed_offline)
    val notPermittedText = stringResource(R.string.members_action_not_permitted)

    LaunchedEffect(viewModel) {
        viewModel.actionFailures.collect { failure ->
            snackbarHostState.showSnackbar(
                when (failure) {
                    RoomAdminResult.NotConnected -> failedOfflineText
                    RoomAdminResult.NotPermitted -> notPermittedText
                    else -> failedText
                },
            )
        }
    }

    Scaffold(
        snackbarHost = { SnackbarHost(snackbarHostState) },
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        text = stringResource(R.string.members_title, name),
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(
                            Icons.AutoMirrored.Filled.ArrowBack,
                            contentDescription = stringResource(R.string.action_back),
                        )
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .fillMaxSize(),
        ) {
            if (state.canManage) {
                AddMemberSearch(state = state, viewModel = viewModel)
            }
            StatusBanner(state = state)
            MemberList(
                state = state,
                onRowClick = { row ->
                    if (state.canManage && !row.inferred &&
                        row.affiliation != WaddleMucAffiliation.OWNER
                    ) {
                        actionTarget = row
                    }
                },
            )
        }
    }

    actionTarget?.let { row ->
        MemberActionSheet(
            row = row,
            onDismiss = { actionTarget = null },
            onSetAffiliation = { affiliation -> viewModel.setAffiliation(row, affiliation) },
            onKick = { pendingAction = PendingAction.Kick(row) },
            onBan = { pendingAction = PendingAction.Ban(row) },
            onRemove = { pendingAction = PendingAction.Remove(row) },
        )
    }

    pendingAction?.let { action ->
        ConfirmActionDialog(
            action = action,
            onConfirm = {
                when (action) {
                    is PendingAction.Kick -> viewModel.kick(action.row)
                    is PendingAction.Ban -> viewModel.ban(action.row)
                    is PendingAction.Remove -> viewModel.remove(action.row)
                }
                pendingAction = null
            },
            onDismiss = { pendingAction = null },
        )
    }
}

@Composable
private fun AddMemberSearch(state: MembersUiState, viewModel: MembersViewModel) {
    OutlinedTextField(
        value = state.searchQuery,
        onValueChange = viewModel::onSearchQueryChanged,
        label = { Text(text = stringResource(R.string.members_search_hint)) },
        singleLine = true,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 4.dp)
            .testTag(MembersScreenTestTags.SEARCH_FIELD),
    )
    state.searchResults.forEach { entry ->
        ListItem(
            headlineContent = { Text(text = entry.displayName ?: entry.username) },
            supportingContent = { Text(text = entry.jid) },
            leadingContent = { Icon(Icons.Outlined.PersonAdd, contentDescription = null) },
            trailingContent = { Text(text = stringResource(R.string.members_add_as_member)) },
            modifier = Modifier.clickable { viewModel.addMember(entry) },
        )
    }
}

@Composable
private fun StatusBanner(state: MembersUiState) {
    when {
        state.status == MemberListStatus.LOADING && state.rows.isEmpty() ->
            CircularProgressIndicator(modifier = Modifier.padding(16.dp))
        state.status == MemberListStatus.UNAVAILABLE -> Text(
            text = stringResource(
                if (state.rows.isEmpty()) {
                    R.string.members_unavailable
                } else {
                    R.string.members_last_synced
                },
            ),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
        )
        else -> Unit
    }
}

@Composable
private fun MemberList(state: MembersUiState, onRowClick: (MemberRow) -> Unit) {
    if (state.rows.isEmpty() && state.status == MemberListStatus.LOADED) {
        Text(
            text = stringResource(R.string.members_empty),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(16.dp),
        )
        return
    }
    LazyColumn {
        items(items = state.rows, key = { "${it.jid ?: it.displayName}:${it.inferred}" }) { row ->
            MemberListRow(row = row, onClick = { onRowClick(row) })
        }
    }
}

@Composable
private fun MemberListRow(row: MemberRow, onClick: () -> Unit) {
    ListItem(
        headlineContent = { Text(text = row.displayName) },
        supportingContent = {
            Column {
                row.jid?.let { Text(text = it, style = MaterialTheme.typography.bodySmall) }
                if (row.hats.isNotEmpty()) {
                    Text(
                        text = row.hats.joinToString(" · "),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.primary,
                    )
                }
            }
        },
        leadingContent = { Icon(Icons.Outlined.AccountCircle, contentDescription = null) },
        trailingContent = {
            Column {
                Text(text = affiliationLabel(row.affiliation))
                if (row.presentNow) {
                    Text(
                        text = stringResource(R.string.members_present_now),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.primary,
                    )
                }
            }
        },
        modifier = Modifier
            .clickable(onClick = onClick)
            .testTag(MembersScreenTestTags.MEMBER_ROW_PREFIX + row.displayName),
    )
}

/** Wire value stays `outcast`; only the label reads "Banned" (web parity). */
@Composable
private fun affiliationLabel(affiliation: WaddleMucAffiliation): String = stringResource(
    when (affiliation) {
        WaddleMucAffiliation.OWNER -> R.string.affiliation_owner
        WaddleMucAffiliation.ADMIN -> R.string.affiliation_admin
        WaddleMucAffiliation.MEMBER -> R.string.affiliation_member
        WaddleMucAffiliation.OUTCAST -> R.string.affiliation_banned
        WaddleMucAffiliation.NONE -> R.string.affiliation_none
    },
)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun MemberActionSheet(
    row: MemberRow,
    onDismiss: () -> Unit,
    onSetAffiliation: (WaddleMucAffiliation) -> Unit,
    onKick: () -> Unit,
    onBan: () -> Unit,
    onRemove: () -> Unit,
) {
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(modifier = Modifier.navigationBarsPadding().padding(bottom = 8.dp)) {
            Text(
                text = row.displayName,
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
            )
            if (row.affiliation != WaddleMucAffiliation.ADMIN) {
                MemberSheetAction(label = stringResource(R.string.members_make_admin)) {
                    onSetAffiliation(WaddleMucAffiliation.ADMIN)
                    onDismiss()
                }
            }
            if (row.affiliation != WaddleMucAffiliation.MEMBER) {
                MemberSheetAction(label = stringResource(R.string.members_make_member)) {
                    onSetAffiliation(WaddleMucAffiliation.MEMBER)
                    onDismiss()
                }
            }
            if (row.presentNow && row.nick != null) {
                MemberSheetAction(
                    label = stringResource(R.string.members_kick),
                    labelColor = MaterialTheme.colorScheme.error,
                ) {
                    onKick()
                    onDismiss()
                }
            }
            if (row.affiliation != WaddleMucAffiliation.OUTCAST) {
                MemberSheetAction(
                    label = stringResource(R.string.members_ban),
                    labelColor = MaterialTheme.colorScheme.error,
                ) {
                    onBan()
                    onDismiss()
                }
            }
            MemberSheetAction(
                label = stringResource(R.string.members_remove),
                labelColor = MaterialTheme.colorScheme.error,
            ) {
                onRemove()
                onDismiss()
            }
        }
    }
}

@Composable
private fun MemberSheetAction(
    label: String,
    labelColor: Color = Color.Unspecified,
    onClick: () -> Unit,
) {
    ListItem(
        headlineContent = { Text(text = label, color = labelColor) },
        colors = ListItemDefaults.colors(containerColor = Color.Transparent),
        modifier = Modifier.clickable(onClick = onClick),
    )
}

@Composable
private fun ConfirmActionDialog(
    action: PendingAction,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    val (titleRes, bodyRes, confirmRes) = when (action) {
        is PendingAction.Kick -> Triple(
            R.string.members_kick_confirm_title,
            R.string.members_kick_confirm_body,
            R.string.members_kick,
        )
        is PendingAction.Ban -> Triple(
            R.string.members_ban_confirm_title,
            R.string.members_ban_confirm_body,
            R.string.members_ban,
        )
        is PendingAction.Remove -> Triple(
            R.string.members_remove_confirm_title,
            R.string.members_remove_confirm_body,
            R.string.members_remove,
        )
    }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(text = stringResource(titleRes)) },
        text = { Text(text = stringResource(bodyRes, action.row.displayName)) },
        confirmButton = {
            TextButton(onClick = onConfirm) {
                Text(
                    text = stringResource(confirmRes),
                    color = MaterialTheme.colorScheme.error,
                )
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text(text = stringResource(R.string.action_cancel))
            }
        },
    )
}

/** Semantics tags shared with instrumented tests. */
object MembersScreenTestTags {
    const val SEARCH_FIELD = "members-search-field"
    const val MEMBER_ROW_PREFIX = "member-row:"
}
