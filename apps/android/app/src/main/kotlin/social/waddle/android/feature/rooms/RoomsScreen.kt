package social.waddle.android.feature.rooms

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Scaffold
import androidx.compose.material3.ScaffoldDefaults
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.feature.home.CreateRoomHost
import social.waddle.android.feature.home.DashedNote
import social.waddle.android.feature.home.HomeViewModel
import social.waddle.android.feature.home.ShellSearchField
import social.waddle.android.feature.home.rememberNow
import social.waddle.android.feature.home.rememberStableOrder
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.BackArrow
import social.waddle.android.ui.WaddleTopBar
import social.waddle.android.ui.huddle.PillButton
import social.waddle.android.ui.huddle.SkeletonBlock
import java.time.Clock

/**
 * The Rooms tab (web `RoomsPage`): every room as a tile, huddles first,
 * then mentions and unread, held still while you look. Search narrows
 * by name; owners get "New room".
 */
@Composable
fun RoomsScreen(
    onOpenChannel: (roomJid: String, name: String) -> Unit,
    contentWindowInsets: WindowInsets = ScaffoldDefaults.contentWindowInsets,
    speaking: Set<String> = emptySet(),
    clock: Clock = Clock.systemDefaultZone(),
    onBack: (() -> Unit)? = null,
) {
    val graph = LocalAppGraph.current
    val viewModel: RoomsViewModel = viewModel(factory = RoomsViewModel.factory(graph))
    // Room creation is the shell's (shared with Home's "+").
    val homeViewModel: HomeViewModel = viewModel(factory = HomeViewModel.factory(graph))
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val canCreate by homeViewModel.canCreateChannel.collectAsStateWithLifecycle()
    val now by rememberNow(clock)
    var query by rememberSaveable { mutableStateOf("") }
    val snackbar = remember { SnackbarHostState() }
    val create = homeViewModel.createRoom

    fun enter(room: RoomActivity) {
        viewModel.openRoom(room.roomJid)
        onOpenChannel(room.roomJid, room.name)
    }
    // The joiner already joined the room: opening it is navigation only.
    val join = rememberHuddleJoiner(onJoined = { room -> onOpenChannel(room.roomJid, room.name) })

    Scaffold(
        contentWindowInsets = contentWindowInsets,
        snackbarHost = { SnackbarHost(snackbar) },
        topBar = {
            WaddleTopBar(
                title = { Text(stringResource(R.string.rooms_browse_title)) },
                navigationIcon = { BackArrow(onBack) },
                actions = {
                    if (canCreate) {
                        NewRoomPill(onCreate = { create.open = true })
                        Spacer(Modifier.size(Waddle.space.xs))
                    }
                },
            )
        },
    ) { padding ->
        RoomsContent(
            state = state,
            query = query,
            onQueryChange = { query = it },
            nowMillis = now.toInstant().toEpochMilli(),
            canCreate = canCreate,
            onCreate = { create.open = true },
            onEnter = ::enter,
            onJoin = join,
            speaking = speaking,
            modifier = Modifier
                .padding(padding)
                .consumeWindowInsets(padding),
        )
    }
    CreateRoomHost(viewModel = homeViewModel, state = create, snackbar = snackbar, onOpenChannel = onOpenChannel)
}

/** Semantics tags shared with instrumented tests. */
object RoomsScreenTestTags {
    const val SCREEN = "rooms-screen"
    const val NEW_ROOM = "rooms-new-room"
}

/** The Rooms tab body, stateless: every value it shows comes in. */
@Composable
fun RoomsContent(
    state: RoomsUiState,
    query: String,
    onQueryChange: (String) -> Unit,
    nowMillis: Long,
    canCreate: Boolean,
    onCreate: () -> Unit,
    onEnter: (RoomActivity) -> Unit,
    onJoin: (RoomActivity) -> Unit,
    modifier: Modifier = Modifier,
    speaking: Set<String> = emptySet(),
) {
    val listState = rememberLazyListState()
    val ordered = rememberStableOrder(
        sorted = state.rooms,
        keyOf = RoomActivity::roomJid,
        interacting = listState.isScrollInProgress,
    )
    val needle = query.trim()
    val visible = if (needle.isEmpty()) ordered else ordered.filter { it.name.contains(needle, ignoreCase = true) }
    LazyColumn(
        state = listState,
        contentPadding = PaddingValues(
            start = Waddle.space.md,
            end = Waddle.space.md,
            top = Waddle.space.xs,
            bottom = Waddle.space.xl,
        ),
        modifier = modifier
            .fillMaxSize()
            .imePadding()
            .testTag(RoomsScreenTestTags.SCREEN),
    ) {
        item(key = "search", contentType = "search") {
            ShellSearchField(
                value = query,
                onValueChange = onQueryChange,
                placeholder = stringResource(R.string.rooms_search),
                modifier = Modifier.padding(bottom = Waddle.space.md),
            )
        }
        when {
            state.rooms.isEmpty() && !state.settled -> loadingRows()
            state.rooms.isEmpty() -> item(key = "empty") {
                DashedNote(text = stringResource(R.string.rooms_empty)) {
                    if (canCreate) NewRoomPill(onCreate)
                }
            }
            visible.isEmpty() -> item(key = "no-match") {
                DashedNote(text = stringResource(R.string.rooms_no_match, needle))
            }
            // One card of rows, as chat lists them: each row draws its
            // slice of the card, so the list stays lazy.
            else -> itemsIndexed(items = visible, key = { _, room -> room.roomJid }, contentType = { _, _ -> "room" }) {
                    index,
                    room,
                ->
                Column(
                    Modifier
                        .animateItem()
                        .clip(cellShape(first = index == 0, last = index == visible.lastIndex))
                        .background(Waddle.colors.surface),
                ) {
                    if (index > 0) HorizontalDivider(color = Waddle.colors.borderSoft)
                    RoomListRow(
                        room = room,
                        nowMillis = nowMillis,
                        onEnter = { onEnter(room) },
                        onJoin = { onJoin(room) },
                        speaking = speaking,
                    )
                }
            }
        }
    }
}

/** The slice of the list card one row draws: rounded at the ends only. */
@Composable
private fun cellShape(first: Boolean, last: Boolean): RoundedCornerShape {
    val r = Waddle.radius.card
    return RoundedCornerShape(
        topStart = if (first) r else 0.dp,
        topEnd = if (first) r else 0.dp,
        bottomStart = if (last) r else 0.dp,
        bottomEnd = if (last) r else 0.dp,
    )
}

@Composable
private fun NewRoomPill(onCreate: () -> Unit) {
    PillButton(
        text = stringResource(R.string.rooms_new),
        onClick = onCreate,
        icon = Icons.Outlined.Add,
        modifier = Modifier.testTag(RoomsScreenTestTags.NEW_ROOM),
    )
}

/** "Finding your rooms." over the shapes of the rows to come. */
private fun LazyListScope.loadingRows() {
    item(key = "loading") { DashedNote(text = stringResource(R.string.rooms_loading)) }
    items(count = SKELETON_ROWS, key = { "skeleton-$it" }, contentType = { "skeleton" }) {
        SkeletonBlock(
            Modifier
                .padding(top = Waddle.space.xs)
                .fillMaxWidth(),
            height = SKELETON_ROW,
            radius = Waddle.radius.control,
        )
    }
}

private val SKELETON_ROW = 56.dp
private const val SKELETON_ROWS = 4
