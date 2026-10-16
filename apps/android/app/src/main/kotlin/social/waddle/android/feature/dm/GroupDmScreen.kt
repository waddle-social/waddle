package social.waddle.android.feature.dm

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Group
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.client.RoomAdminResult
import social.waddle.android.feature.channel.ChannelViewModel
import social.waddle.android.feature.conversation.ConversationKind
import social.waddle.android.feature.conversation.ConversationScreen
import social.waddle.android.feature.conversation.ConversationSheet
import social.waddle.android.feature.conversation.RoomHeaderSubline
import social.waddle.android.feature.conversation.trustedPreviewOriginOf
import social.waddle.android.feature.search.MessageSearchTarget
import social.waddle.android.jid.bareJidOf
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.huddle.AvatarSize
import social.waddle.android.ui.huddle.HuddleAvatar
import social.waddle.client.ffi.WaddleUserSearchEntry

/**
 * Group-DM conversation: the full MUC conversation infra (join on
 * open, room MAM, groupchat sends) with group-specific top-bar
 * actions — members, rename, add member (with the Waddle
 * history-access choice), and leave.
 */
@Composable
fun GroupDmScreen(
    roomJid: String,
    name: String,
    onBack: () -> Unit,
    onOpenThread: (threadId: String) -> Unit,
    onOpenMembers: () -> Unit,
) {
    val graph = LocalAppGraph.current
    // Group DMs are plain hidden MUCs: the channel conversation
    // ViewModel (join + MAM + sends + pins + notify) is reused as-is.
    val viewModel: ChannelViewModel = viewModel(
        key = "group-dm:$roomJid",
        factory = ChannelViewModel.factory(graph, roomJid),
    )
    val session by graph.currentSession.collectAsStateWithLifecycle()
    val groupDms by graph.sessionManager.roomStore.groupDms.collectAsStateWithLifecycle()
    // Renames land via the topology refresh (server-rewritten
    // bookmarks), so the live store name wins over the nav argument.
    val title = groupDms.firstOrNull { it.roomJid == roomJid }?.name ?: name
    val directory by graph.sessionManager.peopleDirectory.collectAsStateWithLifecycle()

    var renameOpen by remember { mutableStateOf(false) }
    var addMemberOpen by remember { mutableStateOf(false) }
    var leaveOpen by remember { mutableStateOf(false) }

    ConversationScreen(
        title = title,
        kind = ConversationKind.Room(title),
        titleSubline = { RoomHeaderSubline(here = directory.rooms[roomJid]?.here, huddle = null) },
        viewModel = viewModel,
        onBack = onBack,
        onOpenThread = onOpenThread,
        searchTarget = MessageSearchTarget(roomJid, isGroupchat = true),
        selfBareJid = session?.jid?.let(::bareJidOf),
        trustedMediaOrigin = trustedPreviewOriginOf(session, graph.serverUrl),
        extraTopBarActions = {
            IconButton(
                onClick = onOpenMembers,
                modifier = Modifier.testTag(GroupDmTestTags.MEMBERS_ACTION),
            ) {
                Icon(
                    Icons.Outlined.Group,
                    contentDescription = stringResource(R.string.members_action),
                )
            }
        },
        overflowTestTag = GroupDmTestTags.MENU_ACTION,
        overflowItems = { dismiss ->
            DropdownMenuItem(
                text = { Text(text = stringResource(R.string.group_dm_rename_action)) },
                onClick = {
                    dismiss()
                    renameOpen = true
                },
                modifier = Modifier.testTag(GroupDmTestTags.RENAME_MENU_ITEM),
            )
            DropdownMenuItem(
                text = { Text(text = stringResource(R.string.group_dm_add_member_action)) },
                onClick = {
                    dismiss()
                    addMemberOpen = true
                },
                modifier = Modifier.testTag(GroupDmTestTags.ADD_MEMBER_MENU_ITEM),
            )
            DropdownMenuItem(
                text = { Text(text = stringResource(R.string.group_dm_leave_action)) },
                onClick = {
                    dismiss()
                    leaveOpen = true
                },
                modifier = Modifier.testTag(GroupDmTestTags.LEAVE_MENU_ITEM),
            )
        },
    )

    if (renameOpen) {
        GroupDmRenameDialog(
            roomJid = roomJid,
            currentName = title,
            onDismiss = { renameOpen = false },
        )
    }
    if (addMemberOpen) {
        GroupDmAddMemberSheet(
            roomJid = roomJid,
            onDismiss = { addMemberOpen = false },
        )
    }
    if (leaveOpen) {
        GroupDmLeaveDialog(
            roomJid = roomJid,
            name = title,
            onDismiss = { leaveOpen = false },
            onLeft = {
                leaveOpen = false
                onBack()
            },
        )
    }
}

/** `urn:waddle:group-dm:rename:0`: edit or clear the display name. */
@Composable
private fun GroupDmRenameDialog(
    roomJid: String,
    currentName: String,
    onDismiss: () -> Unit,
) {
    val graph = LocalAppGraph.current
    val scope = rememberCoroutineScope()
    var name by remember { mutableStateOf(currentName) }
    var failed by remember { mutableStateOf(false) }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(text = stringResource(R.string.group_dm_rename_title)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(Waddle.space.xs)) {
                OutlinedTextField(
                    value = name,
                    onValueChange = {
                        name = it
                        failed = false
                    },
                    label = { Text(text = stringResource(R.string.new_group_dm_name_label)) },
                    singleLine = true,
                    modifier = Modifier
                        .fillMaxWidth()
                        .testTag(GroupDmTestTags.RENAME_FIELD),
                )
                if (failed) {
                    Text(
                        text = stringResource(R.string.action_failed),
                        style = MaterialTheme.typography.bodySmall,
                        color = Waddle.colors.dangerText,
                    )
                }
            }
        },
        confirmButton = {
            TextButton(
                onClick = {
                    scope.launch {
                        val result = graph.sessionManager.renameGroupDm(roomJid, name)
                        if (result == RoomAdminResult.Ok) onDismiss() else failed = true
                    }
                },
                modifier = Modifier.testTag(GroupDmTestTags.RENAME_CONFIRM),
            ) {
                Text(text = stringResource(R.string.action_save))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text(text = stringResource(R.string.action_cancel))
            }
        },
    )
}

/**
 * XEP-0045 §7.8.2 mediated add-member: XEP-0055 search with the
 * history-access choice (default from-join; the full-archive grant is
 * server-validated against the inviter's own visibility).
 */
@Composable
private fun GroupDmAddMemberSheet(
    roomJid: String,
    onDismiss: () -> Unit,
) {
    val graph = LocalAppGraph.current
    val scope = rememberCoroutineScope()
    var query by remember { mutableStateOf("") }
    var results by remember { mutableStateOf(emptyList<WaddleUserSearchEntry>()) }
    var fullHistory by remember { mutableStateOf(false) }
    var failed by remember { mutableStateOf(false) }

    LaunchedEffect(query) {
        val trimmed = query.trim()
        if (trimmed.isEmpty()) {
            results = emptyList()
            return@LaunchedEffect
        }
        delay(NewGroupDmViewModel.SEARCH_DEBOUNCE_MS)
        results = graph.sessionManager.searchUsers(trimmed).orEmpty()
    }

    ConversationSheet(
        onDismissRequest = onDismiss,
        title = stringResource(R.string.group_dm_add_member_title),
        modifier = Modifier.testTag(GroupDmTestTags.ADD_MEMBER_SHEET),
    ) {
        Column(
            modifier = Modifier.fillMaxWidth(),
            verticalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        ) {
            OutlinedTextField(
                value = query,
                onValueChange = {
                    query = it
                    failed = false
                },
                label = { Text(text = stringResource(R.string.members_search_hint)) },
                singleLine = true,
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(GroupDmTestTags.ADD_MEMBER_SEARCH),
            )
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .heightIn(min = Waddle.space.xxl),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
            ) {
                Text(
                    text = stringResource(R.string.group_dm_full_history_toggle),
                    style = Waddle.type.control,
                    modifier = Modifier.weight(1f),
                )
                Switch(
                    checked = fullHistory,
                    onCheckedChange = { fullHistory = it },
                    modifier = Modifier.testTag(GroupDmTestTags.FULL_HISTORY_TOGGLE),
                )
            }
            if (failed) {
                Text(
                    text = stringResource(R.string.action_failed),
                    style = MaterialTheme.typography.bodySmall,
                    color = Waddle.colors.dangerText,
                )
            }
            results.forEach { entry ->
                key(entry.jid) {
                    ListItem(
                        headlineContent = { Text(text = memberLabelOf(entry), style = Waddle.type.control) },
                        supportingContent = {
                            Text(
                                text = entry.jid,
                                style = Waddle.type.time,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        },
                        leadingContent = {
                            HuddleAvatar(
                                jid = entry.jid,
                                name = memberLabelOf(entry),
                                size = AvatarSize.MD,
                                ground = Waddle.colors.surface,
                            )
                        },
                        colors = ListItemDefaults.colors(containerColor = Waddle.colors.surface),
                        modifier = Modifier
                            .testTag(GroupDmTestTags.ADD_MEMBER_RESULT_PREFIX + entry.jid)
                            .clip(RoundedCornerShape(Waddle.radius.control))
                            .clickable {
                                scope.launch {
                                    val result = graph.sessionManager.inviteToGroupDm(
                                        roomJid = roomJid,
                                        inviteeJid = entry.jid,
                                        fullHistory = fullHistory,
                                    )
                                    if (result == RoomAdminResult.Ok) onDismiss() else failed = true
                                }
                            },
                    )
                }
            }
        }
    }
}

/** Confirm-leave: `urn:waddle:group-dm:leave:0`, then pop back. */
@Composable
private fun GroupDmLeaveDialog(
    roomJid: String,
    name: String,
    onDismiss: () -> Unit,
    onLeft: () -> Unit,
) {
    val graph = LocalAppGraph.current
    val scope = rememberCoroutineScope()
    var failed by remember { mutableStateOf(false) }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(text = stringResource(R.string.group_dm_leave_confirm_title)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(Waddle.space.xs)) {
                Text(text = stringResource(R.string.group_dm_leave_confirm_body, name))
                if (failed) {
                    Text(
                        text = stringResource(R.string.action_failed),
                        style = MaterialTheme.typography.bodySmall,
                        color = Waddle.colors.dangerText,
                    )
                }
            }
        },
        confirmButton = {
            TextButton(
                onClick = {
                    scope.launch {
                        val result = graph.sessionManager.leaveGroupDm(roomJid)
                        if (result == RoomAdminResult.Ok) onLeft() else failed = true
                    }
                },
                modifier = Modifier.testTag(GroupDmTestTags.LEAVE_CONFIRM),
            ) {
                Text(
                    text = stringResource(R.string.group_dm_leave_action),
                    color = Waddle.colors.dangerText,
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
object GroupDmTestTags {
    const val MEMBERS_ACTION = "group-dm-members-action"
    const val MENU_ACTION = "group-dm-menu-action"
    const val RENAME_MENU_ITEM = "group-dm-rename-item"
    const val ADD_MEMBER_MENU_ITEM = "group-dm-add-member-item"
    const val LEAVE_MENU_ITEM = "group-dm-leave-item"
    const val RENAME_FIELD = "group-dm-rename-field"
    const val RENAME_CONFIRM = "group-dm-rename-confirm"
    const val ADD_MEMBER_SHEET = "group-dm-add-member-sheet"
    const val ADD_MEMBER_SEARCH = "group-dm-add-member-search"
    const val ADD_MEMBER_RESULT_PREFIX = "group-dm-add-member-result:"
    const val FULL_HISTORY_TOGGLE = "group-dm-full-history-toggle"
    const val LEAVE_CONFIRM = "group-dm-leave-confirm"
}
