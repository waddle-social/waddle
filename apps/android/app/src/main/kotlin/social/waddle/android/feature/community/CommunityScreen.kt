package social.waddle.android.feature.community

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material3.AssistChip
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedCard
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.PrimaryTabRow
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Tab
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
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
import social.waddle.android.client.FeedEntry
import social.waddle.android.client.FeedSourceKind
import social.waddle.android.client.VerbResult
import social.waddle.android.jid.localpartOf
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle

/**
 * XEP-0472 community surface (web `CommunityView` Feed tab parity):
 * newest-first post list with pull-to-refresh, a post composer with
 * optimistic prepend, and PEP-bridge source badges on bridged
 * entries. A single Feed tab for now — the tab row is the mount point
 * for the Events tab of a later train PR.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun CommunityScreen(onBack: () -> Unit) {
    val graph = LocalAppGraph.current
    val session by graph.currentSession.collectAsStateWithLifecycle()
    // Keyed per account (profile-screen parity): logout→login as
    // another account must get a fresh VM and a fresh refresh instead
    // of a cached VM whose init-refresh already ran.
    val viewModel: CommunityViewModel = viewModel(
        key = "community:${session?.jid.orEmpty()}",
        factory = CommunityViewModel.factory(graph),
    )
    val entries by viewModel.entries.collectAsStateWithLifecycle()
    val isLoading by viewModel.isLoading.collectAsStateWithLifecycle()
    val isPosting by viewModel.isPosting.collectAsStateWithLifecycle()
    val loadFailed by viewModel.loadFailed.collectAsStateWithLifecycle()
    val snackbarHostState = remember { SnackbarHostState() }
    var draft by remember { mutableStateOf("") }
    val postFailedText = stringResource(R.string.community_post_failed)

    LaunchedEffect(viewModel) {
        viewModel.postResults.collect { result ->
            when (result) {
                VerbResult.Ok -> draft = ""
                else -> snackbarHostState.showSnackbar(postFailedText)
            }
        }
    }

    Scaffold(
        snackbarHost = { SnackbarHost(snackbarHostState) },
        topBar = {
            TopAppBar(
                title = { Text(text = stringResource(R.string.community_title)) },
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
            PrimaryTabRow(selectedTabIndex = 0) {
                Tab(
                    selected = true,
                    onClick = {},
                    text = { Text(text = stringResource(R.string.community_tab_feed)) },
                    modifier = Modifier.testTag(CommunityTestTags.FEED_TAB),
                )
            }
            if (loadFailed) {
                Text(
                    text = stringResource(R.string.community_feed_load_failed),
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 16.dp, vertical = 4.dp)
                        .testTag(CommunityTestTags.LOAD_ERROR),
                )
            }
            PullToRefreshBox(
                isRefreshing = isLoading,
                onRefresh = viewModel::refresh,
                modifier = Modifier.weight(1f),
            ) {
                if (entries.isEmpty()) {
                    // A failed load must not masquerade as an empty
                    // feed — the banner above owns that story.
                    FeedEmptyState(isLoading = isLoading, showPlaceholder = !loadFailed)
                } else {
                    LazyColumn(
                        modifier = Modifier.fillMaxSize(),
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                        contentPadding = PaddingValues(16.dp),
                    ) {
                        items(items = entries, key = { it.id }) { entry ->
                            FeedEntryCard(entry = entry)
                        }
                    }
                }
            }
            FeedComposer(
                draft = draft,
                onDraftChange = { draft = it },
                isPosting = isPosting,
                onSend = { viewModel.post(draft) },
            )
        }
    }
}

/** Semantics tags shared with instrumented tests. */
object CommunityTestTags {
    const val FEED_TAB = "community-feed-tab"
    const val COMPOSER_FIELD = "community-composer-field"
    const val COMPOSER_SEND = "community-composer-send"
    const val ENTRY_PREFIX = "community-entry:"
    const val LOAD_ERROR = "community-load-error"
}

@Composable
private fun FeedEmptyState(isLoading: Boolean, showPlaceholder: Boolean) {
    // The refresh indicator owns the loading story; only a settled
    // empty feed that actually loaded gets the placeholder text.
    Box(
        modifier = Modifier
            .fillMaxSize()
            .padding(32.dp),
        contentAlignment = Alignment.Center,
    ) {
        if (!isLoading && showPlaceholder) {
            Text(
                text = stringResource(R.string.community_feed_empty),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
        }
    }
}

@Composable
private fun FeedEntryCard(entry: FeedEntry) {
    OutlinedCard(modifier = Modifier.testTag(CommunityTestTags.ENTRY_PREFIX + entry.id)) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    text = entry.author?.let(::localpartOf)
                        ?: stringResource(R.string.community_author_unknown),
                    style = MaterialTheme.typography.titleSmall,
                    modifier = Modifier.weight(1f, fill = false),
                )
                entry.source?.let { source -> SourceBadge(source = source) }
            }
            entry.published?.let { published ->
                Text(
                    text = formatPublished(published),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            // Manual posts default the Atom title to a body preview;
            // only render a title that adds information.
            entry.title
                ?.takeIf { it.isNotBlank() && !entry.body.startsWith(it) }
                ?.let { title ->
                    Text(text = title, style = MaterialTheme.typography.titleMedium)
                }
            Text(text = entry.body, style = MaterialTheme.typography.bodyMedium)
        }
    }
}

/** PEP-bridge kind chip (web `FeedPane` SOURCE_LABELS parity). */
@Composable
private fun SourceBadge(source: FeedSourceKind) {
    AssistChip(
        onClick = {},
        enabled = false,
        label = {
            Text(
                text = stringResource(
                    when (source) {
                        FeedSourceKind.MOOD -> R.string.community_source_mood
                        FeedSourceKind.ACTIVITY -> R.string.community_source_activity
                        FeedSourceKind.TUNE -> R.string.community_source_tune
                        FeedSourceKind.AVATAR -> R.string.community_source_avatar
                        FeedSourceKind.VCARD -> R.string.community_source_vcard
                        FeedSourceKind.RSVP -> R.string.community_source_rsvp
                    },
                ),
                style = MaterialTheme.typography.labelSmall,
            )
        },
    )
}

@Composable
private fun FeedComposer(
    draft: String,
    onDraftChange: (String) -> Unit,
    isPosting: Boolean,
    onSend: () -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        OutlinedTextField(
            value = draft,
            onValueChange = onDraftChange,
            placeholder = { Text(text = stringResource(R.string.community_post_hint)) },
            enabled = !isPosting,
            modifier = Modifier
                .weight(1f)
                .testTag(CommunityTestTags.COMPOSER_FIELD),
        )
        IconButton(
            onClick = onSend,
            enabled = !isPosting && draft.isNotBlank(),
            modifier = Modifier.testTag(CommunityTestTags.COMPOSER_SEND),
        ) {
            Icon(
                Icons.AutoMirrored.Filled.Send,
                contentDescription = stringResource(R.string.community_post_send),
            )
        }
    }
}

private val PUBLISHED_FORMAT: DateTimeFormatter =
    DateTimeFormatter.ofLocalizedDateTime(FormatStyle.SHORT)

private fun formatPublished(published: Instant): String =
    PUBLISHED_FORMAT.format(published.atZone(ZoneId.systemDefault()))
