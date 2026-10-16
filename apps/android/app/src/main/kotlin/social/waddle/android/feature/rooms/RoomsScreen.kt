package social.waddle.android.feature.rooms

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.GridItemSpan
import androidx.compose.foundation.lazy.grid.LazyGridScope
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.lazy.grid.rememberLazyGridState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material3.Scaffold
import androidx.compose.material3.ScaffoldDefaults
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.feature.home.CreateRoomHost
import social.waddle.android.feature.home.CreateRoomState
import social.waddle.android.feature.home.DashedNote
import social.waddle.android.feature.home.HomeViewModel
import social.waddle.android.feature.home.PageHeader
import social.waddle.android.feature.home.ShellSearchField
import social.waddle.android.feature.home.rememberNow
import social.waddle.android.feature.home.rememberStableOrder
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.huddle.HuddleCard
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
    val create = remember { CreateRoomState() }

    fun enter(room: RoomActivity) {
        viewModel.openRoom(room.roomJid)
        onOpenChannel(room.roomJid, room.name)
    }
    // The joiner already joined the room: opening it is navigation only.
    val join = rememberHuddleJoiner(onJoined = { room -> onOpenChannel(room.roomJid, room.name) })

    Scaffold(
        contentWindowInsets = contentWindowInsets,
        snackbarHost = { SnackbarHost(snackbar) },
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
    val gridState = rememberLazyGridState()
    val ordered = rememberStableOrder(
        sorted = state.rooms,
        keyOf = RoomActivity::roomJid,
        interacting = gridState.isScrollInProgress,
    )
    val needle = query.trim()
    val visible = if (needle.isEmpty()) ordered else ordered.filter { it.name.contains(needle, ignoreCase = true) }
    LazyVerticalGrid(
        columns = GridCells.Adaptive(minSize = TILE_MIN_WIDTH),
        state = gridState,
        contentPadding = PaddingValues(
            start = Waddle.space.md,
            end = Waddle.space.md,
            top = Waddle.space.lg,
            bottom = Waddle.space.xl,
        ),
        verticalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = modifier
            .fillMaxSize()
            .imePadding()
            .testTag(RoomsScreenTestTags.SCREEN),
    ) {
        fullWidth("header") {
            RoomsHeader(rooms = state.rooms, canCreate = canCreate, onCreate = onCreate)
        }
        fullWidth("search") {
            ShellSearchField(
                value = query,
                onValueChange = onQueryChange,
                placeholder = stringResource(R.string.rooms_search),
                modifier = Modifier.padding(top = Waddle.space.xs, bottom = Waddle.space.xs),
            )
        }
        when {
            state.rooms.isEmpty() && !state.settled -> loadingTiles()
            state.rooms.isEmpty() -> fullWidth("empty") {
                DashedNote(text = stringResource(R.string.rooms_empty)) {
                    if (canCreate) NewRoomPill(onCreate)
                }
            }
            visible.isEmpty() -> fullWidth("no-match") {
                DashedNote(text = stringResource(R.string.rooms_no_match, needle))
            }
            else -> items(items = visible, key = { it.roomJid }, contentType = { "room" }) { room ->
                RoomTile(
                    room = room,
                    nowMillis = nowMillis,
                    onEnter = { onEnter(room) },
                    onJoin = { onJoin(room) },
                    speaking = speaking,
                    modifier = Modifier.animateItem(),
                )
            }
        }
    }
}

@Composable
private fun RoomsHeader(rooms: List<RoomActivity>, canCreate: Boolean, onCreate: () -> Unit) {
    val live = rooms.count { it.live }
    val count = pluralStringResource(R.plurals.rooms_count, rooms.size, rooms.size)
    val kicker = if (live > 0) "$count · ${pluralStringResource(R.plurals.rooms_live, live, live)}" else count
    PageHeader(
        title = stringResource(R.string.rooms_title),
        kicker = kicker,
        lead = stringResource(R.string.rooms_lead),
        live = live > 0,
        action = { if (canCreate) NewRoomPill(onCreate) },
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

private fun LazyGridScope.fullWidth(key: String, content: @Composable () -> Unit) {
    item(key = key, span = { GridItemSpan(maxLineSpan) }, contentType = key) { content() }
}

/** "Finding your rooms." over the shapes of the tiles to come. */
private fun LazyGridScope.loadingTiles() {
    fullWidth("loading") { DashedNote(text = stringResource(R.string.rooms_loading)) }
    items(count = SKELETON_TILES, key = { "skeleton-$it" }, contentType = { "skeleton" }) {
        HuddleCard(modifier = Modifier.fillMaxWidth()) {
            SkeletonBlock(Modifier.fillMaxWidth(SKELETON_KICKER))
            Spacer(Modifier.size(Waddle.space.xs))
            SkeletonBlock(Modifier.fillMaxWidth(SKELETON_TITLE), height = SKELETON_TITLE_HEIGHT)
            Spacer(Modifier.size(Waddle.space.xs))
            SkeletonBlock(Modifier.fillMaxWidth(SKELETON_BODY))
        }
    }
}

private val TILE_MIN_WIDTH = 320.dp
private val SKELETON_TITLE_HEIGHT = 18.dp
private const val SKELETON_TILES = 3
private const val SKELETON_KICKER = 0.35f
private const val SKELETON_TITLE = 0.55f
private const val SKELETON_BODY = 0.85f
