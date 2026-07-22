package social.waddle.android.feature.dm

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Person
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R

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

    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp)
                .padding(bottom = 24.dp)
                .testTag(NewGroupDmTestTags.SHEET),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                text = stringResource(R.string.new_group_dm_title),
                style = MaterialTheme.typography.titleLarge,
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
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(NewGroupDmTestTags.NAME_FIELD),
            )
            state.selected.forEach { (jid, label) ->
                ListItem(
                    headlineContent = { Text(text = label) },
                    supportingContent = { Text(text = jid) },
                    leadingContent = { Icon(Icons.Outlined.Check, contentDescription = null) },
                    trailingContent = {
                        Icon(
                            Icons.Outlined.Close,
                            contentDescription = stringResource(R.string.new_group_dm_remove_member),
                        )
                    },
                    modifier = Modifier
                        .testTag(NewGroupDmTestTags.SELECTED_ROW_PREFIX + jid)
                        .clickable { viewModel.removeMember(jid) },
                )
            }
            OutlinedTextField(
                value = state.searchQuery,
                onValueChange = viewModel::onSearchQueryChanged,
                label = { Text(text = stringResource(R.string.new_group_dm_search_hint)) },
                singleLine = true,
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(NewGroupDmTestTags.SEARCH_FIELD),
            )
            state.searchResults.forEach { entry ->
                ListItem(
                    headlineContent = { Text(text = memberLabelOf(entry)) },
                    supportingContent = { Text(text = entry.jid) },
                    leadingContent = { Icon(Icons.Outlined.Person, contentDescription = null) },
                    modifier = Modifier
                        .testTag(NewGroupDmTestTags.RESULT_ROW_PREFIX + entry.jid)
                        .clickable { viewModel.toggleMember(entry) },
                )
            }
            if (!state.canCreate && !state.isSubmitting) {
                Text(
                    text = stringResource(R.string.new_group_dm_min_members),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (state.createFailed) {
                Text(
                    text = stringResource(R.string.action_failed),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
            }
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End),
            ) {
                TextButton(onClick = onDismiss) {
                    Text(text = stringResource(R.string.action_cancel))
                }
                Button(
                    onClick = { viewModel.create(onCreated) },
                    enabled = state.canCreate,
                    modifier = Modifier.testTag(NewGroupDmTestTags.CREATE_BUTTON),
                ) {
                    Text(text = stringResource(R.string.new_group_dm_create))
                }
            }
        }
    }
}

/** Semantics tags shared with instrumented tests. */
object NewGroupDmTestTags {
    const val SHEET = "new-group-dm-sheet"
    const val NAME_FIELD = "new-group-dm-name"
    const val SEARCH_FIELD = "new-group-dm-search"
    const val CREATE_BUTTON = "new-group-dm-create"
    const val SELECTED_ROW_PREFIX = "new-group-dm-selected:"
    const val RESULT_ROW_PREFIX = "new-group-dm-result:"
}
