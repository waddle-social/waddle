package social.waddle.android.feature.home

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Menu
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.ScaffoldDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.client.ConnectionState
import social.waddle.android.client.normalizedBareJid
import social.waddle.android.feature.people.PersonRow
import social.waddle.android.feature.people.aroundCountOf
import social.waddle.android.feature.people.personRowsOf
import social.waddle.android.feature.people.whoIsHereOf
import social.waddle.android.feature.rooms.RoomActivity
import social.waddle.android.feature.rooms.rememberHuddleJoiner
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.BackArrow
import social.waddle.android.ui.WaddleTopBar
import java.time.Clock
import java.time.ZonedDateTime

/** Where Home can send the user. */
class HomeActions(
    val onOpenRoom: (RoomActivity) -> Unit = {},
    val onJoin: (RoomActivity) -> Unit = {},
    val onOpenPerson: (PersonRow) -> Unit = {},
    val onOpenPeople: () -> Unit = {},
    val onBrowseRooms: () -> Unit = {},
    val onRetry: () -> Unit = {},
    /** Back to the sidebar (wide layouts); `null` hides the arrow. */
    val onBack: (() -> Unit)? = null,
    /** Opens the sidebar drawer (phones, folded); wins over [onBack]. */
    val onOpenMenu: (() -> Unit)? = null,
)

/** Everything Home shows, resolved. */
data class HomeContentState(
    /** What the greeting calls the account holder; `null` greets without a name. */
    val greetingName: String?,
    val rooms: List<RoomActivity>,
    val people: List<PersonRow>,
    val dmUnread: Int,
    val connection: ConnectionState,
    val settled: Boolean,
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
    onOpenPeople: () -> Unit = {},
    onOpenRooms: () -> Unit = {},
    speaking: Set<String> = emptySet(),
    clock: Clock = Clock.systemDefaultZone(),
    onBack: (() -> Unit)? = null,
    onOpenMenu: (() -> Unit)? = null,
) {
    val graph = LocalAppGraph.current
    val viewModel: HomeViewModel = viewModel(factory = HomeViewModel.factory(graph))
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val session by graph.currentSession.collectAsStateWithLifecycle()
    val vcard by graph.sessionManager.profileStore.selfVcard.collectAsStateWithLifecycle()
    val now by rememberNow(clock)

    fun open(room: RoomActivity) {
        viewModel.openChannel(room.roomJid)
        onOpenChannel(room.roomJid, room.name)
    }
    // The joiner already joined the room: opening it is navigation only.
    val join = rememberHuddleJoiner(onJoined = { room -> onOpenChannel(room.roomJid, room.name) })
    val roomNames = state.rooms.associate { normalizedBareJid(it.roomJid) to it.name }

    Scaffold(contentWindowInsets = contentWindowInsets) { padding ->
        HomeContent(
            state = HomeContentState(
                greetingName = greetingNameOf(vcard?.nickname, vcard?.fullName, session?.username),
                rooms = state.rooms,
                people = personRowsOf(state.people, roomNames, speaking),
                dmUnread = state.dmUnreadCount,
                connection = state.connectionState,
                settled = state.settled,
            ),
            now = now,
            actions = HomeActions(
                onOpenRoom = ::open,
                onJoin = join,
                onOpenPerson = { onOpenDm(it.jid, it.name) },
                onOpenPeople = onOpenPeople,
                onBrowseRooms = onOpenRooms,
                onRetry = viewModel::retryConnection,
                onBack = onBack,
                onOpenMenu = onOpenMenu,
            ),
            speaking = speaking,
            modifier = Modifier
                .padding(padding)
                .consumeWindowInsets(padding),
        )
    }
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
    const val MENU = "home-menu"
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
        HomeHeader(actions = actions)
        ConnectionBanner(state = state.connection, onRetry = actions.onRetry)
        HomeBody(state = state, now = now, actions = actions, speaking = speaking)
    }
}

/** Chat's mobile bar: ☰ for the sidebar on phones, else back to it, and the page's name. */
@Composable
private fun HomeHeader(actions: HomeActions) {
    WaddleTopBar(
        title = { Text(stringResource(R.string.tab_home)) },
        navigationIcon = {
            val openMenu = actions.onOpenMenu
            if (openMenu != null) {
                IconButton(onClick = openMenu, modifier = Modifier.testTag(HomeScreenTestTags.MENU)) {
                    Icon(Icons.Outlined.Menu, contentDescription = stringResource(R.string.sidebar_open))
                }
            } else {
                BackArrow(actions.onBack)
            }
        },
    )
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
