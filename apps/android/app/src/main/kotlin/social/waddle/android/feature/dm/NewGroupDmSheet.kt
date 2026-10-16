package social.waddle.android.feature.dm

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.PersonAdd
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.HuddleFieldShape
import social.waddle.android.ui.SectionKicker
import social.waddle.android.ui.SheetTitle
import social.waddle.android.ui.huddle.AvatarSize
import social.waddle.android.ui.huddle.HuddleAvatar
import social.waddle.android.ui.huddle.PillButton
import social.waddle.android.ui.huddleFieldColors

/**
 * Create-group-DM sheet (web `NewGroupDmDialog` parity): XEP-0055
 * multi-select member search, an optional name defaulting to the
 * comma-joined member labels, and the create submit. [initialMembers]
 * prefills the selection (the "start a group from this DM" entry).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun NewGroupDmSheet(
    onDismiss: () -> Unit,
    onCreated: (roomJid: String, name: String) -> Unit,
    initialMembers: Map<String, String> = emptyMap(),
) {
    val graph = LocalAppGraph.current
    val viewModel: NewGroupDmViewModel = viewModel(
        key = "new-group-dm:${initialMembers.keys.sorted().joinToString(",")}",
        factory = NewGroupDmViewModel.factory(graph, initialMembers),
    )
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val colors = Waddle.colors

    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = colors.surface) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .navigationBarsPadding()
                .imePadding()
                .verticalScroll(rememberScrollState())
                .padding(bottom = Waddle.space.lg)
                .testTag(NewGroupDmTestTags.SHEET),
        ) {
            SheetTitle(
                text = stringResource(R.string.new_group_dm_title),
                supporting = stringResource(R.string.huddle_new_group_supporting),
            )
            OutlinedTextField(
                value = state.name,
                onValueChange = viewModel::onNameChanged,
                label = { Text(text = stringResource(R.string.new_group_dm_name_label)) },
                placeholder = {
                    Text(
                        text = state.defaultName.ifEmpty {
                            stringResource(R.string.new_group_dm_name_placeholder)
                        },
                    )
                },
                singleLine = true,
                shape = HuddleFieldShape,
                colors = huddleFieldColors(),
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = Waddle.space.md)
                    .testTag(NewGroupDmTestTags.NAME_FIELD),
            )
            if (state.selected.isNotEmpty()) {
                SectionKicker(
                    text = pluralStringResource(
                        R.plurals.huddle_new_group_selected,
                        state.selected.size,
                        state.selected.size,
                    ),
                )
                state.selected.forEach { (jid, label) ->
                    key(jid) {
                        PersonPickRow(
                            jid = jid,
                            name = label,
                            modifier = Modifier.testTag(NewGroupDmTestTags.SELECTED_ROW_PREFIX + jid),
                            trailing = {
                                IconButton(onClick = { viewModel.removeMember(jid) }) {
                                    Icon(
                                        Icons.Outlined.Close,
                                        contentDescription = stringResource(R.string.new_group_dm_remove_member),
                                        tint = colors.fgMuted,
                                    )
                                }
                            },
                        )
                    }
                }
            }
            OutlinedTextField(
                value = state.searchQuery,
                onValueChange = viewModel::onSearchQueryChanged,
                placeholder = { Text(text = stringResource(R.string.new_group_dm_search_hint)) },
                leadingIcon = { Icon(Icons.Outlined.Search, contentDescription = null) },
                singleLine = true,
                shape = HuddleFieldShape,
                colors = huddleFieldColors(),
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = Waddle.space.md)
                    .padding(top = Waddle.space.md)
                    .testTag(NewGroupDmTestTags.SEARCH_FIELD),
            )
            state.searchResults.forEach { entry ->
                key(entry.jid) {
                    val picked = entry.jid in state.selected
                    PersonPickRow(
                        jid = entry.jid,
                        name = memberLabelOf(entry),
                        onClick = { viewModel.toggleMember(entry) },
                        modifier = Modifier.testTag(NewGroupDmTestTags.RESULT_ROW_PREFIX + entry.jid),
                        trailing = {
                            Icon(
                                if (picked) Icons.Outlined.CheckCircle else Icons.Outlined.PersonAdd,
                                contentDescription = null,
                                tint = if (picked) colors.actionText else colors.fgSoft,
                            )
                        },
                    )
                }
            }
            if (state.createFailed) {
                Text(
                    text = stringResource(R.string.action_failed),
                    style = Waddle.type.control,
                    color = colors.dangerText,
                    modifier = Modifier.padding(horizontal = Waddle.space.md, vertical = Waddle.space.xs),
                )
            }
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = Waddle.space.md)
                    .padding(top = Waddle.space.lg),
            ) {
                Text(
                    text = if (state.canCreate || state.isSubmitting) {
                        ""
                    } else {
                        stringResource(R.string.new_group_dm_min_members)
                    },
                    style = Waddle.type.control.copy(fontWeight = null),
                    color = colors.fgMuted,
                    modifier = Modifier.weight(1f),
                )
                TextButton(onClick = onDismiss) {
                    Text(text = stringResource(R.string.action_cancel), color = colors.fgMuted)
                }
                PillButton(
                    text = stringResource(R.string.new_group_dm_create),
                    onClick = { viewModel.create(onCreated) },
                    enabled = state.canCreate,
                    modifier = Modifier.testTag(NewGroupDmTestTags.CREATE_BUTTON),
                )
            }
        }
    }
}

/** A person in the picker: avatar, name, JID; [trailing] says picked or removes. */
@Composable
private fun PersonPickRow(
    jid: String,
    name: String,
    modifier: Modifier = Modifier,
    onClick: (() -> Unit)? = null,
    trailing: @Composable () -> Unit,
) {
    val colors = Waddle.colors
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = modifier
            .fillMaxWidth()
            .then(if (onClick != null) Modifier.clickable(onClick = onClick) else Modifier)
            .heightIn(min = PICK_ROW)
            .padding(start = Waddle.space.md, end = Waddle.space.xs),
    ) {
        HuddleAvatar(jid = jid, name = name, size = AvatarSize.MD, ground = colors.surface)
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = name,
                style = Waddle.type.body,
                color = colors.fg,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                text = jid,
                style = Waddle.type.time,
                color = colors.fgMuted,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        trailing()
    }
}

private val PICK_ROW = 56.dp

/** Semantics tags shared with instrumented tests. */
object NewGroupDmTestTags {
    const val SHEET = "new-group-dm-sheet"
    const val NAME_FIELD = "new-group-dm-name"
    const val SEARCH_FIELD = "new-group-dm-search"
    const val CREATE_BUTTON = "new-group-dm-create"
    const val SELECTED_ROW_PREFIX = "new-group-dm-selected:"
    const val RESULT_ROW_PREFIX = "new-group-dm-result:"
}
