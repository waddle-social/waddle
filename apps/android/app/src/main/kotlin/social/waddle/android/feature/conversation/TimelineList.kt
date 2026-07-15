package social.waddle.android.feature.conversation

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import social.waddle.android.client.store.TimelineItem

/**
 * Reverse-layout timeline (newest pinned at the bottom); reaching the
 * top (the end in reverse coordinates) requests older history.
 */
@Composable
fun TimelineList(
    rows: List<ConversationRow>,
    isLoadingOlder: Boolean,
    onTopReached: () -> Unit,
    onRetry: (Long) -> Unit,
    modifier: Modifier = Modifier,
    pinnedIds: Set<String> = emptySet(),
    onLongPress: (item: TimelineItem) -> Unit = {},
    onToggleReaction: (item: TimelineItem, emoji: String) -> Unit = { _, _ -> },
) {
    val listState = rememberLazyListState()
    // reverseLayout renders index 0 at the bottom → newest first.
    val newestFirst = remember(rows) { rows.asReversed() }

    LazyColumn(
        state = listState,
        reverseLayout = true,
        modifier = modifier.fillMaxWidth(),
        contentPadding = PaddingValues(vertical = 8.dp),
    ) {
        items(items = newestFirst, key = ::rowKey) { row ->
            MessageCard(
                row = row,
                onRetry = onRetry,
                pinnedIds = pinnedIds,
                onLongPress = onLongPress,
                onToggleReaction = onToggleReaction,
            )
        }
        if (isLoadingOlder) {
            item(key = LOADING_KEY) {
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(vertical = 8.dp),
                    contentAlignment = Alignment.Center,
                ) {
                    CircularProgressIndicator(modifier = Modifier.size(24.dp))
                }
            }
        }
    }

    LaunchedEffect(listState, newestFirst.size) {
        snapshotFlow { listState.layoutInfo.visibleItemsInfo.lastOrNull()?.index }
            .collect { lastVisible ->
                val nearTop = lastVisible != null &&
                    newestFirst.isNotEmpty() &&
                    lastVisible >= newestFirst.lastIndex - LOAD_MORE_THRESHOLD
                if (nearTop) {
                    onTopReached()
                }
            }
    }
}

private fun rowKey(row: ConversationRow): String = when (row) {
    is ConversationRow.Stored -> "s:${row.item.id}"
    is ConversationRow.Unconfirmed -> "p:${row.message.localId}"
}

private const val LOAD_MORE_THRESHOLD = 3
private const val LOADING_KEY = "loading-older"
