package social.waddle.android.feature.admin

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.outlined.AccountCircle
import androidx.compose.material3.Badge
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.jid.localpartOf
import social.waddle.client.ffi.WaddleAdminUserEntry

/**
 * Read-only community-admin Users list (web `UsersPanel.vue` parity):
 * display name, JID, and the owner pill for `has_owner_hat` rows.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun CommunityUsersScreen(onBack: () -> Unit) {
    val graph = LocalAppGraph.current
    val viewModel: CommunityUsersViewModel = viewModel(factory = CommunityUsersViewModel.factory(graph))
    val state by viewModel.uiState.collectAsStateWithLifecycle()

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(text = stringResource(R.string.community_users_title)) },
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
            OutlinedTextField(
                value = state.query,
                onValueChange = viewModel::onQueryChanged,
                label = { Text(text = stringResource(R.string.community_users_search_hint)) },
                singleLine = true,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 4.dp)
                    .testTag(CommunityUsersTestTags.SEARCH_FIELD),
            )
            if (state.failed) {
                Text(
                    text = stringResource(R.string.community_users_failed),
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.padding(16.dp),
                )
            }
            LazyColumn {
                items(items = state.entries, key = { it.jid }) { entry ->
                    UserRow(entry = entry)
                }
                if (state.nextCursor != null) {
                    item(key = "load-more") {
                        TextButton(
                            onClick = viewModel::loadMore,
                            enabled = !state.loading,
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(8.dp),
                        ) {
                            Text(text = stringResource(R.string.community_users_load_more))
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun UserRow(entry: WaddleAdminUserEntry) {
    ListItem(
        headlineContent = { Text(text = entry.displayName ?: localpartOf(entry.jid)) },
        supportingContent = { Text(text = entry.jid) },
        leadingContent = { Icon(Icons.Outlined.AccountCircle, contentDescription = null) },
        trailingContent = {
            if (entry.hasOwnerHat) {
                Badge { Text(text = stringResource(R.string.community_users_owner_pill)) }
            }
        },
    )
}

/** Semantics tags shared with instrumented tests. */
object CommunityUsersTestTags {
    const val SEARCH_FIELD = "community-users-search"
}
