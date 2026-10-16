package social.waddle.android.feature.admin

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.Group
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.jid.localpartOf
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.EmptyState
import social.waddle.android.ui.GlyphTile
import social.waddle.android.ui.HuddleFieldShape
import social.waddle.android.ui.WaddleTopBar
import social.waddle.android.ui.dangerSubtle
import social.waddle.android.ui.huddle.AvatarSize
import social.waddle.android.ui.huddle.HuddleAvatar
import social.waddle.android.ui.huddle.HuddleTag
import social.waddle.android.ui.huddle.PillButton
import social.waddle.android.ui.huddle.PillTone
import social.waddle.android.ui.huddle.SkeletonBlock
import social.waddle.android.ui.huddle.TagTone
import social.waddle.android.ui.huddleFieldColors
import social.waddle.client.ffi.WaddleAdminUserEntry

/**
 * Read-only community-admin Users list (web `UsersPanel.vue` parity):
 * display name, JID, and the gold owner tag for `has_owner_hat` rows.
 */
@Composable
fun CommunityUsersScreen(onBack: () -> Unit) {
    val graph = LocalAppGraph.current
    val viewModel: CommunityUsersViewModel = viewModel(factory = CommunityUsersViewModel.factory(graph))
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    CommunityUsersContent(
        state = state,
        actions = CommunityUsersActions(
            onBack = onBack,
            onQueryChanged = viewModel::onQueryChanged,
            onLoadMore = viewModel::loadMore,
            onRetry = viewModel::loadFirstPage,
        ),
    )
}

/** What the users list can ask for. */
internal data class CommunityUsersActions(
    val onBack: () -> Unit = {},
    val onQueryChanged: (String) -> Unit = {},
    val onLoadMore: () -> Unit = {},
    val onRetry: () -> Unit = {},
)

/** Stateless users list: search, then loading, a problem, nobody, or the people. */
@Composable
internal fun CommunityUsersContent(state: CommunityUsersUiState, actions: CommunityUsersActions) {
    Scaffold(
        containerColor = Waddle.colors.bg,
        topBar = {
            WaddleTopBar(
                title = { Text(text = stringResource(R.string.community_users_title)) },
                navigationIcon = {
                    IconButton(onClick = actions.onBack) {
                        Icon(
                            Icons.AutoMirrored.Outlined.ArrowBack,
                            contentDescription = stringResource(R.string.action_back),
                        )
                    }
                },
            )
        },
    ) { padding ->
        LazyColumn(
            contentPadding = PaddingValues(
                top = padding.calculateTopPadding(),
                bottom = padding.calculateBottomPadding() + Waddle.space.xl,
            ),
            modifier = Modifier
                .consumeWindowInsets(padding)
                // Results stay above the keyboard while searching.
                .imePadding(),
        ) {
            item(key = "search") {
                OutlinedTextField(
                    value = state.query,
                    onValueChange = actions.onQueryChanged,
                    placeholder = { Text(text = stringResource(R.string.community_users_search_hint)) },
                    leadingIcon = { Icon(Icons.Outlined.Search, contentDescription = null) },
                    singleLine = true,
                    shape = HuddleFieldShape,
                    colors = huddleFieldColors(),
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = Waddle.space.md, vertical = Waddle.space.xs)
                        .testTag(CommunityUsersTestTags.SEARCH_FIELD),
                )
            }
            if (state.failed) {
                item(key = "failed") { ProblemPanel(onRetry = actions.onRetry) }
            }
            when {
                state.loading && state.entries.isEmpty() ->
                    items(count = SKELETON_ROWS, key = { "skeleton:$it" }) { UserRowSkeleton() }
                state.entries.isEmpty() && !state.failed -> item(key = "empty") {
                    val query = state.query.trim()
                    EmptyState(
                        icon = if (query.isEmpty()) Icons.Outlined.Group else Icons.Outlined.Search,
                        title = if (query.isEmpty()) {
                            stringResource(R.string.huddle_users_empty_title)
                        } else {
                            stringResource(R.string.huddle_users_no_match_title, query)
                        },
                        body = stringResource(
                            if (query.isEmpty()) {
                                R.string.huddle_users_empty_body
                            } else {
                                R.string.huddle_users_no_match_body
                            },
                        ),
                    )
                }
                else -> items(items = state.entries, key = { it.jid }) { entry -> UserRow(entry = entry) }
            }
            if (state.nextCursor != null && state.entries.isNotEmpty()) {
                item(key = "load-more") {
                    Box(
                        contentAlignment = Alignment.Center,
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(Waddle.space.md),
                    ) {
                        PillButton(
                            text = stringResource(R.string.community_users_load_more),
                            onClick = actions.onLoadMore,
                            enabled = !state.loading,
                            tone = PillTone.QUIET,
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun UserRow(entry: WaddleAdminUserEntry) {
    val colors = Waddle.colors
    val name = entry.displayName ?: localpartOf(entry.jid)
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = Modifier
            .fillMaxWidth()
            .semantics(mergeDescendants = true) {}
            .heightIn(min = USER_ROW)
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.xs),
    ) {
        HuddleAvatar(jid = entry.jid, name = name, size = AvatarSize.MD)
        Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs)) {
            Text(
                text = name,
                style = Waddle.type.body,
                color = colors.fg,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                text = entry.jid,
                style = Waddle.type.time,
                color = colors.fgMuted,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        if (entry.hasOwnerHat) {
            HuddleTag(text = stringResource(R.string.community_users_owner_pill), tone = TagTone.GOLD)
        }
    }
}

@Composable
private fun UserRowSkeleton() {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = USER_ROW)
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.xs),
    ) {
        SkeletonBlock(
            height = AvatarSize.MD.dp,
            radius = Waddle.radius.pill,
            modifier = Modifier.size(AvatarSize.MD.dp),
        )
        Column {
            SkeletonBlock(modifier = Modifier.width(SKELETON_NAME))
            SkeletonBlock(modifier = Modifier.width(SKELETON_JID))
        }
    }
}

/** The list could not load: say so plainly, offer the retry. */
@Composable
private fun ProblemPanel(onRetry: () -> Unit) {
    val colors = Waddle.colors
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.xs),
    ) {
        GlyphTile(icon = Icons.Outlined.ErrorOutline, tint = colors.dangerText, container = colors.dangerSubtle())
        Column(modifier = Modifier.weight(1f)) {
            Text(text = stringResource(R.string.community_users_failed), style = Waddle.type.body, color = colors.fg)
            Text(
                text = stringResource(R.string.huddle_users_failed_body),
                style = Waddle.type.control.copy(fontWeight = null),
                color = colors.fgMuted,
            )
        }
        TextButton(onClick = onRetry) {
            Text(text = stringResource(R.string.action_retry), color = colors.actionText)
        }
    }
}

private const val SKELETON_ROWS = 6
private val USER_ROW = 56.dp
private val SKELETON_NAME = 132.dp
private val SKELETON_JID = 196.dp

/** Semantics tags shared with instrumented tests. */
object CommunityUsersTestTags {
    const val SEARCH_FIELD = "community-users-search"
}
