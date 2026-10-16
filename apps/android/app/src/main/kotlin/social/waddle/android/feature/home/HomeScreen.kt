package social.waddle.android.feature.home

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.ScaffoldDefaults
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.avatar.PeerAvatar
import social.waddle.android.client.ConnectionState
import social.waddle.android.client.normalizedBareJid
import social.waddle.android.feature.people.PersonRow
import social.waddle.android.feature.people.aroundCountOf
import social.waddle.android.feature.people.personRowsOf
import social.waddle.android.feature.people.whoIsHereOf
import social.waddle.android.feature.rooms.RoomActivity
import social.waddle.android.feature.rooms.rememberHuddleJoiner
import social.waddle.android.jid.bareJidOf
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.huddle.AvatarSize
import java.time.Clock
import java.time.ZonedDateTime

/** Where Home can send the user. */
class HomeActions(
    val onOpenRoom: (RoomActivity) -> Unit = {},
    val onJoin: (RoomActivity) -> Unit = {},
    val onOpenPerson: (PersonRow) -> Unit = {},
    val onOpenYou: () -> Unit = {},
    val onOpenPeople: () -> Unit = {},
    val onBrowseRooms: () -> Unit = {},
    val onCreateRoom: () -> Unit = {},
    val onRetry: () -> Unit = {},
)

/** Everything Home shows, resolved. */
data class HomeContentState(
    /** What the greeting calls the account holder; `null` greets without a name. */
    val greetingName: String?,
    val accountName: String,
    val accountJid: String?,
    val rooms: List<RoomActivity>,
    val people: List<PersonRow>,
    val dmUnread: Int,
    val connection: ConnectionState,
    val settled: Boolean,
    val canCreateRoom: Boolean,
)

/**
 * The Home tab (web `HomeDashboard`, phone artboard): who is here, a
 * greeting with what is waiting, what is happening now, and the rooms
 * that need you. The clock is a parameter so the greeting is testable.
 */
@Composable
fun HomeScreen(
    onOpenChannel: (roomJid: String, name: String) -> Unit,
    contentWindowInsets: WindowInsets = ScaffoldDefaults.contentWindowInsets,
    onOpenDm: (peerJid: String, name: String) -> Unit = { _, _ -> },
    onOpenYou: () -> Unit = {},
    onOpenPeople: () -> Unit = {},
    onOpenRooms: () -> Unit = {},
    speaking: Set<String> = emptySet(),
    clock: Clock = Clock.systemDefaultZone(),
) {
    val graph = LocalAppGraph.current
    val viewModel: HomeViewModel = viewModel(factory = HomeViewModel.factory(graph))
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val canCreate by viewModel.canCreateChannel.collectAsStateWithLifecycle()
    val session by graph.currentSession.collectAsStateWithLifecycle()
    val vcard by graph.sessionManager.profileStore.selfVcard.collectAsStateWithLifecycle()
    val now by rememberNow(clock)
    val snackbar = remember { SnackbarHostState() }
    val create = viewModel.createRoom

    fun open(room: RoomActivity) {
        viewModel.openChannel(room.roomJid)
        onOpenChannel(room.roomJid, room.name)
    }
    // The joiner already joined the room: opening it is navigation only.
    val join = rememberHuddleJoiner(onJoined = { room -> onOpenChannel(room.roomJid, room.name) })
    val accountJid = session?.jid?.let(::bareJidOf)
    val accountName = vcard?.fullName?.takeIf { it.isNotBlank() }
        ?: vcard?.nickname?.takeIf { it.isNotBlank() }
        ?: session?.username
        ?: accountJid.orEmpty()
    val roomNames = state.rooms.associate { normalizedBareJid(it.roomJid) to it.name }

    Scaffold(contentWindowInsets = contentWindowInsets, snackbarHost = { SnackbarHost(snackbar) }) { padding ->
        HomeContent(
            state = HomeContentState(
                greetingName = greetingNameOf(vcard?.nickname, vcard?.fullName, session?.username),
                accountName = accountName,
                accountJid = accountJid,
                rooms = state.rooms,
                people = personRowsOf(state.people, roomNames, speaking),
                dmUnread = state.dmUnreadCount,
                connection = state.connectionState,
                settled = state.settled,
                canCreateRoom = canCreate,
            ),
            now = now,
            actions = HomeActions(
                onOpenRoom = ::open,
                onJoin = join,
                onOpenPerson = { onOpenDm(it.jid, it.name) },
                onOpenYou = onOpenYou,
                onOpenPeople = onOpenPeople,
                onBrowseRooms = onOpenRooms,
                onCreateRoom = { create.open = true },
                onRetry = viewModel::retryConnection,
            ),
            speaking = speaking,
            modifier = Modifier
                .padding(padding)
                .consumeWindowInsets(padding),
        )
    }
    CreateRoomHost(viewModel = viewModel, state = create, snackbar = snackbar, onOpenChannel = onOpenChannel)
}

/**
 * What the greeting calls you: the nickname you chose, else the first
 * word of your full name, else your account name.
 */
fun greetingNameOf(nickname: String?, fullName: String?, username: String?): String? =
    nickname?.trim()?.takeIf { it.isNotEmpty() }
        ?: fullName?.trim()?.split(WHITESPACE)?.firstOrNull()?.takeIf { it.isNotEmpty() }
        ?: username?.trim()?.takeIf { it.isNotEmpty() }

private val WHITESPACE = Regex("\\s+")

/** Semantics tags shared with instrumented tests. */
object HomeScreenTestTags {
    const val SCREEN = "home-screen"
    const val CREATE_CHANNEL_ACTION = "home-create-channel-action"
    const val ACCOUNT_ACTION = "home-account-action"
}

/** The Home tab, stateless: header, connection, then the scrolling body. */
@Composable
fun HomeContent(
    state: HomeContentState,
    now: ZonedDateTime,
    actions: HomeActions,
    modifier: Modifier = Modifier,
    speaking: Set<String> = emptySet(),
) {
    Column(modifier = modifier.fillMaxSize().testTag(HomeScreenTestTags.SCREEN)) {
        HomeHeader(state = state, actions = actions)
        ConnectionBanner(state = state.connection, onRetry = actions.onRetry)
        HomeBody(state = state, now = now, actions = actions, speaking = speaking)
    }
}

/** `waddle.` on the left; "+" for owners and your avatar (to You) on the right. */
@Composable
private fun HomeHeader(state: HomeContentState, actions: HomeActions) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = HEADER_HEIGHT)
            .padding(start = Waddle.space.md, end = Waddle.space.xxs),
    ) {
        Wordmark(modifier = Modifier.weight(1f))
        if (state.canCreateRoom) {
            IconButton(
                onClick = actions.onCreateRoom,
                modifier = Modifier.testTag(HomeScreenTestTags.CREATE_CHANNEL_ACTION),
            ) {
                Icon(Icons.Outlined.Add, contentDescription = stringResource(R.string.create_channel_title))
            }
        }
        val you = stringResource(R.string.shell_account_action)
        IconButton(
            onClick = actions.onOpenYou,
            modifier = Modifier
                .testTag(HomeScreenTestTags.ACCOUNT_ACTION)
                .semantics { contentDescription = you },
        ) {
            PeerAvatar(jid = state.accountJid, displayName = state.accountName, size = AvatarSize.MD.dp)
        }
    }
}

@Composable
private fun HomeBody(
    state: HomeContentState,
    now: ZonedDateTime,
    actions: HomeActions,
    speaking: Set<String>,
) {
    val listState = rememberLazyListState()
    val loading = !state.settled && state.rooms.isEmpty()
    val strip = rememberStableOrder(
        sorted = whoIsHereOf(state.people),
        keyOf = PersonRow::jid,
        interacting = listState.isScrollInProgress,
    )
    val sections = homeSectionsOf(state.rooms)
    val around = aroundCountOf(state.people)
    val hero = HeroModel(
        now = now,
        name = state.greetingName,
        around = around,
        inHuddle = state.rooms.sumOf { it.huddle.size },
        anyLive = sections.live.isNotEmpty(),
        summary = summaryOf(state.rooms, state.dmUnread, around),
        action = heroActionOf(sections.live, sections.busiest + sections.needsYou),
        loading = loading,
    )
    LazyColumn(
        state = listState,
        contentPadding = PaddingValues(
            start = Waddle.space.md,
            end = Waddle.space.md,
            top = Waddle.space.xs,
            bottom = Waddle.space.xl,
        ),
        modifier = Modifier.fillMaxSize(),
    ) {
        if (strip.isNotEmpty()) {
            item(key = "strip", contentType = "strip") {
                WhoIsHereStrip(
                    people = strip,
                    onOpen = actions.onOpenPerson,
                    onMore = actions.onOpenPeople,
                    modifier = Modifier.padding(bottom = Waddle.space.md),
                )
            }
        }
        item(key = "hero", contentType = "hero") {
            HomeHero(
                model = hero,
                onAction = { action ->
                    when (action) {
                        is HeroAction.Join -> actions.onJoin(action.room)
                        is HeroAction.Return -> actions.onOpenRoom(action.room)
                        is HeroAction.JumpInto -> actions.onOpenRoom(action.room)
                        HeroAction.Browse -> actions.onBrowseRooms()
                    }
                },
            )
        }
        if (loading) {
            items(count = SKELETON_CARDS, key = { "skeleton-$it" }) {
                SkeletonCard(modifier = Modifier.padding(top = Waddle.space.sm))
            }
        } else {
            happeningNow(sections, now, actions, speaking)
            needsYou(sections.needsYou, actions)
        }
    }
}

private fun LazyListScope.happeningNow(
    sections: HomeSections,
    now: ZonedDateTime,
    actions: HomeActions,
    speaking: Set<String>,
) {
    if (sections.live.isEmpty() && sections.busiest.isEmpty()) return
    item(key = "happening", contentType = "label") {
        val live = sections.live.size
        SectionLabel(
            text = stringResource(R.string.home_happening_title),
            live = true,
            trailing = if (live > 0) pluralStringResource(R.plurals.home_happening_live, live, live) else null,
            modifier = Modifier.padding(top = Waddle.space.xl, bottom = Waddle.space.sm),
        )
    }
    items(items = sections.live, key = { "live-${it.roomJid}" }, contentType = { "live" }) { room ->
        LiveHuddleCard(
            room = room,
            onOpen = { actions.onOpenRoom(room) },
            onJoin = { actions.onJoin(room) },
            speaking = speaking,
            modifier = Modifier
                .animateItem()
                .padding(bottom = Waddle.space.sm),
        )
    }
    items(items = sections.busiest, key = { "busy-${it.roomJid}" }, contentType = { "busy" }) { room ->
        BusyRoomCard(
            room = room,
            now = now,
            onOpen = { actions.onOpenRoom(room) },
            modifier = Modifier
                .animateItem()
                .padding(bottom = Waddle.space.sm),
        )
    }
}

private fun LazyListScope.needsYou(rooms: List<RoomActivity>, actions: HomeActions) {
    if (rooms.isEmpty()) return
    item(key = "needs", contentType = "label") {
        SectionLabel(
            text = stringResource(R.string.home_needs_title),
            modifier = Modifier.padding(top = Waddle.space.lg, bottom = Waddle.space.sm),
        )
    }
    items(items = rooms, key = { "needs-${it.roomJid}" }, contentType = { "needs" }) { room ->
        NeedsYouRow(
            room = room,
            onOpen = { actions.onOpenRoom(room) },
            modifier = Modifier
                .animateItem()
                .padding(bottom = Waddle.space.xs),
        )
    }
}

private const val SKELETON_CARDS = 2
private val HEADER_HEIGHT = Waddle.control.touch + Waddle.space.md
