package social.waddle.android.feature.conversation

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.launch
import social.waddle.android.R
import social.waddle.android.theme.Waddle
import java.time.LocalDate
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.util.Locale

/**
 * Reverse-layout timeline (newest pinned at the bottom). Rows arrive
 * fully derived ([FeedEntry]); this only draws them. Reaching the top
 * (the end in reverse coordinates) requests older history.
 */
@Composable
fun TimelineList(
    entries: List<FeedEntry>,
    isLoadingOlder: Boolean,
    reachedHistoryStart: Boolean,
    onTopReached: () -> Unit,
    actions: MessageRowActions,
    context: MessageRowContext,
    modifier: Modifier = Modifier,
    onAtNewestEdgeChanged: (Boolean) -> Unit = {},
    /** "Today" for separator labels; injectable so screenshots are stable. */
    today: LocalDate = LocalDate.now(ZoneId.systemDefault()),
) {
    val listState = rememberLazyListState()
    val scope = rememberCoroutineScope()
    // reverseLayout renders index 0 at the bottom → newest first.
    val newestFirst = remember(entries) { entries.asReversed() }
    val indexByKey = remember(entries) {
        HashMap<String, Int>(newestFirst.size).apply { newestFirst.forEachIndexed { i, e -> put(e.key, i) } }
    }
    val jumpActions = remember(actions, indexByKey) {
        MessageRowActions(
            onLongPress = actions.onLongPress,
            onToggleReaction = actions.onToggleReaction,
            onOpenThread = actions.onOpenThread,
            onRetry = actions.onRetry,
            onQuoteClick = { parent ->
                indexByKey["s:${parent.presentationId}"]?.let { index ->
                    scope.launch { listState.animateScrollToItem(index) }
                }
            },
        )
    }

    Box(modifier = modifier.fillMaxWidth()) {
        when {
            entries.isEmpty() && isLoadingOlder -> TimelineSkeleton()
            entries.isEmpty() -> EmptyTimeline()
            else -> LazyColumn(
                state = listState,
                reverseLayout = true,
                modifier = Modifier.fillMaxSize(),
                contentPadding = PaddingValues(top = Waddle.space.md, bottom = Waddle.space.xs),
            ) {
                items(
                    items = newestFirst,
                    key = { it.key },
                    contentType = { entry ->
                        when {
                            entry.isCallRow -> CONTENT_CALL
                            entry.startsGroup -> CONTENT_GROUP_START
                            else -> CONTENT_CONTINUATION
                        }
                    },
                ) { entry ->
                    // Item content is not reversed: separators sit above their row.
                    Column {
                        entry.daySeparator?.let { DaySeparator(it, today) }
                        if (entry.unreadDivider) UnreadDivider()
                        val stored = (entry.row as? ConversationRow.Stored)?.item
                        if (entry.isCallRow && stored != null) {
                            CallTimelineRow(item = stored)
                        } else {
                            MessageRow(entry = entry, actions = jumpActions, context = context)
                        }
                    }
                }
                if (isLoadingOlder) {
                    item(key = LOADING_KEY, contentType = LOADING_KEY) {
                        Box(
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(vertical = Waddle.space.sm),
                            contentAlignment = Alignment.Center,
                        ) {
                            CircularProgressIndicator(modifier = Modifier.size(20.dp), strokeWidth = 2.dp)
                        }
                    }
                } else if (reachedHistoryStart) {
                    item(key = START_KEY, contentType = START_KEY) { StartOfHistory() }
                }
            }
        }

        val awayFromLive by remember {
            derivedStateOf { listState.firstVisibleItemIndex > JUMP_THRESHOLD }
        }
        AnimatedVisibility(
            visible = awayFromLive,
            enter = fadeIn(tween(MOTION_FAST)) + slideInVertically(tween(MOTION_FAST)) { it / 2 },
            exit = fadeOut(tween(MOTION_FAST)) + slideOutVertically(tween(MOTION_FAST)) { it / 2 },
            modifier = Modifier
                .align(Alignment.BottomCenter)
                .padding(bottom = Waddle.space.sm),
        ) {
            JumpToLatest(onClick = { scope.launch { listState.animateScrollToItem(0) } })
        }
    }

    // Web isPinnedAtEdge parity: in reverse layout, item 0 visible ==
    // scrolled to the newest message. Read markers gate on this.
    LaunchedEffect(listState) {
        snapshotFlow { listState.firstVisibleItemIndex == 0 }
            .distinctUntilChanged()
            .collect(onAtNewestEdgeChanged)
    }

    LaunchedEffect(listState, newestFirst.size) {
        snapshotFlow { listState.layoutInfo.visibleItemsInfo.lastOrNull()?.index }
            .collect { lastVisible ->
                val nearTop = lastVisible != null &&
                    newestFirst.isNotEmpty() &&
                    lastVisible >= newestFirst.lastIndex - LOAD_MORE_THRESHOLD
                if (nearTop) onTopReached()
            }
    }
}

/** Hairline · "Today" · hairline. */
@Composable
fun DaySeparator(day: LocalDate, today: LocalDate = LocalDate.now(ZoneId.systemDefault())) {
    val label = when (day) {
        today -> stringResource(R.string.timeline_today)
        today.minusDays(1) -> stringResource(R.string.timeline_yesterday)
        else -> dayLabelOf(day, today, LocalConfiguration.current.locales[0])
    }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.sm)
            .semantics(mergeDescendants = true) { heading() },
    ) {
        Rule(Modifier.weight(1f), Waddle.colors.hairline)
        Text(
            text = label,
            style = MaterialTheme.typography.labelMedium,
            fontWeight = FontWeight.SemiBold,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Rule(Modifier.weight(1f), Waddle.colors.hairline)
    }
}

/** Where the messages that were unread on open begin; ember, because it is news. */
@Composable
fun UnreadDivider() {
    val description = stringResource(R.string.timeline_unread_divider_description)
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
        modifier = Modifier
            .fillMaxWidth()
            .padding(start = Waddle.space.md, end = Waddle.space.md, top = Waddle.space.xs)
            .semantics(mergeDescendants = true) { contentDescription = description },
    ) {
        Rule(Modifier.weight(1f), Waddle.colors.live)
        Text(
            text = stringResource(R.string.timeline_unread_divider).uppercase(),
            style = MaterialTheme.typography.labelSmall,
            fontWeight = FontWeight.Bold,
            color = Waddle.colors.liveText,
        )
    }
}

@Composable
private fun Rule(modifier: Modifier, color: Color) {
    Box(
        modifier = modifier
            .height(1.dp)
            .background(color),
    )
}

@Composable
private fun JumpToLatest(onClick: () -> Unit) {
    Surface(
        onClick = onClick,
        shape = CircleShape,
        color = MaterialTheme.colorScheme.surfaceContainer,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
        shadowElevation = 6.dp,
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
            modifier = Modifier.padding(horizontal = 14.dp, vertical = 7.dp),
        ) {
            Icon(Icons.Filled.KeyboardArrowDown, contentDescription = null, modifier = Modifier.size(16.dp))
            Text(
                text = stringResource(R.string.timeline_jump_to_latest),
                style = MaterialTheme.typography.labelMedium,
                fontWeight = FontWeight.SemiBold,
            )
        }
    }
}

@Composable
private fun StartOfHistory() {
    Text(
        text = stringResource(R.string.timeline_start_of_history),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(horizontal = Waddle.space.md, vertical = Waddle.space.lg),
    )
}

@Composable
private fun EmptyTimeline() {
    Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        Text(
            text = stringResource(R.string.timeline_empty),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/** Placeholder rows while the first history page loads (web `MessageListSkeleton`). */
@Composable
fun TimelineSkeleton(modifier: Modifier = Modifier) {
    val block = MaterialTheme.colorScheme.surfaceContainerHigh
    Column(
        modifier = modifier
            .fillMaxSize()
            .padding(Waddle.space.md),
        verticalArrangement = Arrangement.spacedBy(Waddle.space.lg, Alignment.Bottom),
    ) {
        SKELETON_WIDTHS.forEach { widths ->
            Row(horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm)) {
                Box(
                    Modifier
                        .size(36.dp)
                        .background(block, CircleShape),
                )
                Column(verticalArrangement = Arrangement.spacedBy(Waddle.space.xs)) {
                    Box(
                        Modifier
                            .width(96.dp)
                            .height(10.dp)
                            .background(block, RoundedCornerShape(Waddle.radius.xs)),
                    )
                    widths.forEach { w ->
                        Box(
                            Modifier
                                .width(w.dp)
                                .height(12.dp)
                                .background(block, RoundedCornerShape(Waddle.radius.xs)),
                        )
                    }
                }
            }
        }
        Spacer(Modifier.height(Waddle.space.xs))
    }
}

private fun dayLabelOf(day: LocalDate, today: LocalDate, locale: Locale): String {
    val pattern = if (day.year == today.year) "EEE d MMM" else null
    return if (pattern != null) {
        DateTimeFormatter.ofPattern(pattern, locale).format(day)
    } else {
        DateTimeFormatter.ofLocalizedDate(FormatStyle.MEDIUM).withLocale(locale).format(day)
    }
}

private val SKELETON_WIDTHS = listOf(listOf(220, 140), listOf(260), listOf(180, 230, 120), listOf(200))

private const val LOAD_MORE_THRESHOLD = 3
private const val JUMP_THRESHOLD = 6
private const val MOTION_FAST = 160
private const val LOADING_KEY = "loading-older"
private const val START_KEY = "history-start"
private const val CONTENT_CALL = "call"
private const val CONTENT_GROUP_START = "group-start"
private const val CONTENT_CONTINUATION = "continuation"
