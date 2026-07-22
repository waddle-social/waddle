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
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
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
    threadReplyCounts: Map<String, Int> = emptyMap(),
    onLongPress: (item: TimelineItem) -> Unit = {},
    onToggleReaction: (item: TimelineItem, emoji: String) -> Unit = { _, _ -> },
    onOpenThread: ((item: TimelineItem) -> Unit)? = null,
    onAtNewestEdgeChanged: (Boolean) -> Unit = {},
    selfBareJid: String? = null,
) {
    val listState = rememberLazyListState()
    val scope = rememberCoroutineScope()
    // reverseLayout renders index 0 at the bottom → newest first.
    val newestFirst = remember(rows) { rows.asReversed() }
    // Every wire identity → row, for quote previews and scroll targets.
    val byIdentity = remember(rows) {
        buildMap {
            rows.forEach { row ->
                if (row is ConversationRow.Stored) {
                    put(row.item.id, row.item)
                    row.item.identityIds.forEach { put(it, row.item) }
                }
            }
        }
    }

    LazyColumn(
        state = listState,
        reverseLayout = true,
        modifier = modifier.fillMaxWidth(),
        contentPadding = PaddingValues(vertical = 8.dp),
    ) {
        items(items = newestFirst, key = ::rowKey) { row ->
            // Call anchors render as a dedicated compact row, never as
            // a (possibly bodyless) message bubble.
            val storedItem = (row as? ConversationRow.Stored)?.item
            if (storedItem != null && (storedItem.callAnchor != null || storedItem.callEndedMarker != null)) {
                CallTimelineRow(item = storedItem)
                return@items
            }
            MessageCard(
                row = row,
                onRetry = onRetry,
                pinnedIds = pinnedIds,
                onLongPress = onLongPress,
                onToggleReaction = onToggleReaction,
                resolveQuoted = { id -> byIdentity[id] },
                onQuoteClick = { id ->
                    val target = byIdentity[id] ?: return@MessageCard
                    val index = newestFirst.indexOfFirst { candidate ->
                        candidate is ConversationRow.Stored && candidate.item.id == target.id
                    }
                    if (index >= 0) scope.launch { listState.animateScrollToItem(index) }
                },
                // A root's replies key their <thread/> by ANY of the
                // root's wire identities.
                threadReplyCount = (row as? ConversationRow.Stored)?.item?.let { item ->
                    (listOfNotNull(item.threadId) + item.identityIds)
                        .firstNotNullOfOrNull { threadReplyCounts[it] }
                } ?: 0,
                onOpenThread = onOpenThread,
                selfBareJid = selfBareJid,
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

    // Web isPinnedAtEdge parity: in reverse layout, item 0 visible ==
    // scrolled to the newest message. Read markers gate on this.
    LaunchedEffect(listState) {
        snapshotFlow { listState.firstVisibleItemIndex == 0 }
            .collect(onAtNewestEdgeChanged)
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
    // id + sender: the store deliberately keeps cross-sender id
    // collisions as distinct rows (suppressing them would be an
    // injection vector), and same-sender same-id always merges — so
    // the pair is unique where the id alone would crash the LazyColumn.
    is ConversationRow.Stored -> "s:${row.item.id}:${row.item.from.orEmpty()}"
    is ConversationRow.Unconfirmed -> "p:${row.message.localId}"
}

private const val LOAD_MORE_THRESHOLD = 3
private const val LOADING_KEY = "loading-older"
