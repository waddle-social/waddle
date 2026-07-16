package social.waddle.android.feature.search

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import social.waddle.android.R
import java.time.Instant
import java.time.OffsetDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle

/**
 * Full-text search over one conversation's archive: an autofocused
 * query field above the debounced result list. Results are ephemeral
 * previews (author / body / timestamp) — the sheet never touches the
 * timeline.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MessageSearchSheet(
    viewModel: MessageSearchViewModel,
    onDismiss: () -> Unit,
) {
    val query by viewModel.query.collectAsStateWithLifecycle()
    val state by viewModel.state.collectAsStateWithLifecycle()
    val focusRequester = remember { FocusRequester() }
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(
            modifier = Modifier
                .navigationBarsPadding()
                .imePadding(),
        ) {
            OutlinedTextField(
                value = query,
                onValueChange = viewModel::onQueryChanged,
                // Label, not just placeholder: TalkBack keeps announcing
                // the field's purpose after text is typed (web parity
                // with the input's aria-label).
                label = { Text(stringResource(R.string.search_messages)) },
                placeholder = { Text(stringResource(R.string.search_placeholder)) },
                leadingIcon = { Icon(Icons.Outlined.Search, contentDescription = null) },
                singleLine = true,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp)
                    .focusRequester(focusRequester),
            )
            // Web parity: the caret drops straight into the field on
            // open — without this the user taps the input a second time.
            // The frame yield lets the sheet's dialog-hosted content
            // attach the focus target first; requesting focus on a
            // not-yet-attached node throws.
            LaunchedEffect(Unit) {
                withFrameNanos {}
                focusRequester.requestFocus()
            }
            SearchStateContent(state)
        }
    }
}

@Composable
private fun SearchStateContent(state: MessageSearchState) {
    when (state) {
        MessageSearchState.Idle -> SearchStatusText(stringResource(R.string.search_hint))
        MessageSearchState.Searching -> SearchingRow()
        MessageSearchState.Empty -> SearchStatusText(stringResource(R.string.search_no_results))
        MessageSearchState.Failed -> SearchStatusText(stringResource(R.string.search_failed))
        is MessageSearchState.Results -> LazyColumn {
            items(state.hits, key = { it.key }) { hit ->
                SearchResultRow(hit)
            }
        }
    }
}

/** Loading state: spinner beside the copy so the request reads as in flight. */
@Composable
private fun SearchingRow() {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 24.dp),
    ) {
        CircularProgressIndicator(modifier = Modifier.size(16.dp), strokeWidth = 2.dp)
        Text(
            text = stringResource(R.string.search_searching),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/** Centered single-line status (idle hint, empty, failed). */
@Composable
private fun SearchStatusText(text: String) {
    Box(
        contentAlignment = Alignment.Center,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 24.dp),
    ) {
        Text(
            text = text,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/** Compact result preview: author + stamp header, two-line body. */
@Composable
private fun SearchResultRow(hit: MessageSearchHit) {
    Column(
        verticalArrangement = Arrangement.spacedBy(2.dp),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 8.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                text = hit.author ?: stringResource(R.string.message_unknown_sender),
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.primary,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.padding(end = 8.dp),
            )
            formatSearchStamp(hit.timestamp)?.let {
                Text(
                    text = it,
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        Text(
            text = hit.body,
            style = MaterialTheme.typography.bodyMedium,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/** Date-bearing, unlike the timeline's time-only stamp: archive matches span days. */
private val STAMP_FORMAT: DateTimeFormatter =
    DateTimeFormatter.ofLocalizedDateTime(FormatStyle.SHORT)

/** MessageCard-parity parsing: instant first, offset form as fallback. */
private fun formatSearchStamp(timestamp: String?): String? {
    timestamp ?: return null
    val instant = runCatching { Instant.parse(timestamp) }.getOrElse {
        runCatching { OffsetDateTime.parse(timestamp).toInstant() }.getOrNull()
    } ?: return null
    return STAMP_FORMAT.withZone(ZoneId.systemDefault()).format(instant)
}
