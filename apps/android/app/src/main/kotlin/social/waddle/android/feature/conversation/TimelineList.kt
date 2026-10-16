package social.waddle.android.feature.conversation

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ArrowDownward
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.snapshots.Snapshot
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.launch
import social.waddle.android.R
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.huddle.AvatarSize
import social.waddle.android.ui.huddle.HuddleMotion
import social.waddle.android.ui.huddle.Kicker
import social.waddle.android.ui.huddle.SkeletonBox
import social.waddle.android.ui.huddle.reducedMotion
import java.time.LocalDate

/**
 * Reverse-layout timeline (newest pinned at the bottom). Rows arrive
 * fully derived ([FeedEntry]); this only draws them. Reaching the top
 * (the end in reverse coordinates) requests older history.
 *
 * While the first page loads it shows six skeleton rows; once history is
 * known to be empty it shows [emptyState]. Times and day labels follow
 * the device ([rememberConversationClock]); [today] pins the date for
 * screenshots.
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
    today: LocalDate? = null,
    /** The newest page has not answered yet: skeleton, not the empty state. */
    loadingFirstPage: Boolean = false,
    emptyState: @Composable () -> Unit = { TimelineEmpty(ConversationKind.Room(name = null)) },
) {
    val deviceClock = rememberConversationClock()
    val clock = remember(deviceClock, today) { today?.let(deviceClock::withToday) ?: deviceClock }
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
    val arrivals = rememberArrivals(newestFirst)

    CompositionLocalProvider(LocalConversationClock provides clock) {
        Box(modifier = modifier.fillMaxWidth()) {
            when {
                entries.isEmpty() && (isLoadingOlder || loadingFirstPage) -> TimelineSkeleton()
                entries.isEmpty() -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { emptyState() }
                else -> TimelineRows(
                    newestFirst = newestFirst,
                    listState = listState,
                    isLoadingOlder = isLoadingOlder,
                    reachedHistoryStart = reachedHistoryStart,
                    arrivals = arrivals,
                    actions = jumpActions,
                    context = context,
                )
            }
            val awayFromLive by remember {
                derivedStateOf { listState.firstVisibleItemIndex > JUMP_THRESHOLD }
            }
            AnimatedVisibility(
                visible = awayFromLive,
                enter = fadeIn(HuddleMotion.fast()) + slideInVertically(HuddleMotion.fast()) { it / 2 },
                exit = fadeOut(HuddleMotion.fast()) + slideOutVertically(HuddleMotion.fast()) { it / 2 },
                modifier = Modifier
                    .align(Alignment.BottomCenter)
                    .padding(bottom = Waddle.space.md),
            ) {
                JumpToLatest(onClick = { scope.launch { listState.animateScrollToItem(0) } })
            }
        }
    }

    FollowNewest(listState, newestFirst.firstOrNull())

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

@Composable
private fun TimelineRows(
    newestFirst: List<FeedEntry>,
    listState: LazyListState,
    isLoadingOlder: Boolean,
    reachedHistoryStart: Boolean,
    arrivals: Arrivals,
    actions: MessageRowActions,
    context: MessageRowContext,
) {
    val still = reducedMotion
    LazyColumn(
        state = listState,
        reverseLayout = true,
        modifier = Modifier.fillMaxSize(),
        contentPadding = PaddingValues(top = Waddle.space.md, bottom = Waddle.space.xs),
    ) {
        itemsIndexed(
            items = newestFirst,
            key = { _, entry -> entry.key },
            contentType = { _, entry ->
                when {
                    entry.isCallRow -> CONTENT_CALL
                    entry.startsGroup -> CONTENT_GROUP_START
                    else -> CONTENT_CONTINUATION
                }
            },
        ) { index, entry ->
            // Item content is not reversed: separators sit above their row.
            // The next row in time is the one below, at index - 1.
            val endsGroup = index == 0 || newestFirst[index - 1].startsGroup
            // Latched per row: once drawn, the row is no longer an arrival.
            val fresh = remember(entry.key) { arrivals.isFresh(entry, index) && !still }
            val enter = rememberMessageEnter(fresh)
            Column(
                modifier = Modifier
                    .animateItem(
                        fadeInSpec = null,
                        placementSpec = if (still) null else HuddleMotion.fast(),
                        fadeOutSpec = if (still) null else HuddleMotion.fast(),
                    )
                    .enterLayer(enter),
            ) {
                entry.daySeparator?.let { DaySeparator(it) }
                if (entry.unreadDivider) UnreadDivider()
                val stored = (entry.row as? ConversationRow.Stored)?.item
                if (entry.isCallRow && stored != null) {
                    CallTimelineRow(item = stored, sentAt = entry.sentAt)
                } else {
                    MessageRow(entry = entry, actions = actions, context = context, endsGroup = endsGroup)
                }
            }
        }
        if (isLoadingOlder) {
            item(key = LOADING_KEY, contentType = LOADING_KEY) {
                Column(Modifier.padding(vertical = Waddle.space.sm)) {
                    SkeletonRow(lines = SKELETON_LINES.first())
                }
            }
        } else if (reachedHistoryStart) {
            item(key = START_KEY, contentType = START_KEY) { StartOfHistory() }
        }
    }
}

/**
 * Which rows are arrivals worth animating: rows the list had not drawn
 * before, at the newest edge, after the first page was on screen. Older
 * pages and the initial load land still; an own message's server echo
 * replaces a row that already animated, so it lands still too.
 */
private class Arrivals {
    val seen = HashSet<String>()

    fun isFresh(entry: FeedEntry, index: Int): Boolean {
        if (seen.isEmpty() || entry.key in seen || index > FRESH_WINDOW) return false
        val stored = (entry.row as? ConversationRow.Stored)?.item
        return stored == null || !stored.isMine
    }
}

@Composable
private fun rememberArrivals(newestFirst: List<FeedEntry>): Arrivals {
    val arrivals = remember { Arrivals() }
    SideEffect { newestFirst.forEach { arrivals.seen += it.key } }
    return arrivals
}

/** Web `message-in`: 200ms, rising 6dp from .995 scale, ease-out. Runs once per row; null when still. */
@Composable
private fun rememberMessageEnter(fresh: Boolean): Animatable<Float, *>? {
    if (!fresh) return null
    val progress = remember { Animatable(0f) }
    LaunchedEffect(Unit) { progress.animateTo(1f, tween(MESSAGE_ENTER_MILLIS, easing = HuddleMotion.easing)) }
    return progress
}

/** Reads [progress] in the layer: the entrance redraws, never recomposes. */
private fun Modifier.enterLayer(progress: Animatable<Float, *>?): Modifier = if (progress == null) {
    this
} else {
    graphicsLayer {
        val p = progress.value
        alpha = p
        translationY = (1f - p) * MESSAGE_RISE.toPx()
        val scale = ENTER_SCALE + (1f - ENTER_SCALE) * p
        scaleX = scale
        scaleY = scale
    }
}

/**
 * Keeps the newest message in view as messages arrive. A keyed lazy list
 * holds its first visible item in place when rows are inserted before
 * it, so in reverse layout a new message can land below the viewport.
 * When the reader was at the newest message, or has just sent one, the
 * list follows it down; a reader scrolled back through history stays put.
 *
 * "Was at the newest" is read during composition, before the new row is
 * measured, so it reflects the layout the reader was looking at. Only a
 * send that appears while the list is on screen forces the jump: a
 * pending row already there when the list is (re)created, say after a
 * rotation restored a history position, does not. The scroll runs in the
 * composition's scope, so the echo replacing the pending row (a new key)
 * does not cancel a jump still in flight.
 */
@Composable
private fun FollowNewest(listState: LazyListState, newest: FeedEntry?) {
    val scope = rememberCoroutineScope()
    val seen = remember { NewestSeen() }
    val newestKey = newest?.key
    val follow = remember(newestKey) {
        val wasAtNewest = Snapshot.withoutReadObservation { listState.firstVisibleItemIndex == 0 }
        val freshSend = seen.key != null && newest?.row is ConversationRow.Unconfirmed
        wasAtNewest || freshSend
    }
    SideEffect { seen.key = newestKey }
    LaunchedEffect(newestKey) {
        if (newestKey != null && follow) scope.launch { listState.animateScrollToItem(0) }
    }
}

/** The newest row key the list last composed with; null until the first. */
private class NewestSeen {
    var key: String? = null
}

/** Rule · "TODAY" · rule: a mono kicker between hairlines. */
@Composable
fun DaySeparator(day: LocalDate) {
    val clock = rememberConversationClock()
    val label = clock.dayText(clock.labelOf(day))
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.sm)
            .semantics(mergeDescendants = true) { heading() },
    ) {
        Rule(Modifier.weight(1f), Waddle.colors.borderSoft)
        Kicker(text = label)
        Rule(Modifier.weight(1f), Waddle.colors.borderSoft)
    }
}

/**
 * Where the messages that were unread on open begin: a teal "New
 * messages" pill between teal rules. Teal, not ember: it marks your
 * place, it is not live.
 */
@Composable
fun UnreadDivider() {
    val colors = Waddle.colors
    val description = stringResource(R.string.timeline_unread_divider_description)
    val rule = colors.action.copy(alpha = UNREAD_RULE_ALPHA)
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.xs)
            .semantics(mergeDescendants = true) { contentDescription = description },
    ) {
        Rule(Modifier.weight(1f), rule)
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
            modifier = Modifier
                .background(colors.action.copy(alpha = UNREAD_FILL_ALPHA), CircleShape)
                .border(1.dp, colors.action.copy(alpha = UNREAD_BORDER_ALPHA), CircleShape)
                .padding(horizontal = Waddle.space.sm, vertical = Waddle.space.xxs),
        ) {
            Box(
                Modifier
                    .size(UNREAD_DOT)
                    .background(colors.action, CircleShape),
            )
            Kicker(text = stringResource(R.string.timeline_unread_divider), color = colors.actionText)
        }
        Rule(Modifier.weight(1f), rule)
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

/** Centred "↓ Latest" pill on the card colour, lifted by the overlay shadow. */
@Composable
private fun JumpToLatest(onClick: () -> Unit) {
    val colors = Waddle.colors
    Surface(
        onClick = onClick,
        shape = CircleShape,
        color = colors.surface,
        contentColor = colors.fg,
        border = BorderStroke(1.dp, colors.border),
        modifier = Modifier.overlayShadow(JUMP_HEIGHT / 2),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
            modifier = Modifier
                .height(JUMP_HEIGHT)
                .padding(horizontal = Waddle.space.sm),
        ) {
            Icon(Icons.Outlined.ArrowDownward, contentDescription = null, modifier = Modifier.size(JUMP_ICON))
            Text(
                text = stringResource(R.string.timeline_jump_to_latest),
                style = MetaStyle.copy(fontWeight = FontWeight.SemiBold),
            )
        }
    }
}

@Composable
private fun StartOfHistory() {
    Text(
        text = stringResource(R.string.timeline_start_of_history),
        style = Waddle.type.time,
        color = Waddle.colors.fgMuted,
        textAlign = TextAlign.Center,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.lg),
    )
}

/** Six placeholder rows while the first history page loads (web `MessageListSkeleton`). */
@Composable
fun TimelineSkeleton(modifier: Modifier = Modifier) {
    Column(
        modifier = modifier
            .fillMaxSize()
            .padding(vertical = Waddle.space.md),
        verticalArrangement = Arrangement.spacedBy(Waddle.space.sm, Alignment.Bottom),
    ) {
        SKELETON_LINES.forEach { lines -> SkeletonRow(lines) }
    }
}

/** One skeleton message: avatar, a name bar and [lines] of body. */
@Composable
private fun SkeletonRow(lines: List<Float>) {
    Row(
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.xs),
    ) {
        SkeletonBox(modifier = Modifier.size(AvatarSize.MESSAGE.dp), radius = AvatarSize.MESSAGE.dp / 2)
        Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(Waddle.space.xs)) {
            SkeletonBox(modifier = Modifier.size(width = SKELETON_NAME, height = SKELETON_NAME_HEIGHT))
            lines.forEach { fraction ->
                SkeletonBox(
                    modifier = Modifier
                        .fillMaxWidth(fraction)
                        .height(SKELETON_LINE_HEIGHT),
                )
            }
        }
    }
}

/** Body-line widths of the six skeleton rows, as fractions of the column. */
private val SKELETON_LINES = listOf(
    listOf(0.82f, 0.5f),
    listOf(0.94f),
    listOf(0.64f, 0.88f, 0.4f),
    listOf(0.72f),
    listOf(0.9f, 0.58f),
    listOf(0.46f),
)

private val SKELETON_NAME = 96.dp
private val SKELETON_NAME_HEIGHT = 10.dp
private val SKELETON_LINE_HEIGHT = 12.dp
private val UNREAD_DOT = 6.dp
private val JUMP_HEIGHT = Waddle.control.sm
private val JUMP_ICON = 16.dp
private val MESSAGE_RISE = 6.dp
private const val MESSAGE_ENTER_MILLIS = 200
private const val ENTER_SCALE = 0.995f
private const val UNREAD_FILL_ALPHA = 0.1f
private const val UNREAD_BORDER_ALPHA = 0.3f
private const val UNREAD_RULE_ALPHA = 0.35f
private const val FRESH_WINDOW = 2
private const val LOAD_MORE_THRESHOLD = 3
private const val JUMP_THRESHOLD = 6
private const val LOADING_KEY = "loading-older"
private const val START_KEY = "history-start"
private const val CONTENT_CALL = "call"
private const val CONTENT_GROUP_START = "group-start"
private const val CONTENT_CONTINUATION = "continuation"
