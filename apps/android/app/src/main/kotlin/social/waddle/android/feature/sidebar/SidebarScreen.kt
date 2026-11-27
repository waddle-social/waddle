package social.waddle.android.feature.sidebar

import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.Chat
import androidx.compose.material.icons.automirrored.outlined.List
import androidx.compose.material.icons.outlined.Event
import androidx.compose.material.icons.outlined.Explore
import androidx.compose.material.icons.outlined.Groups
import androidx.compose.material.icons.outlined.People
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.Tag
import androidx.compose.material.icons.outlined.Tune
import androidx.compose.material.icons.outlined.UnfoldMore
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.feature.account.AccountSheet
import social.waddle.android.feature.dm.DmListItem
import social.waddle.android.feature.dm.DmListRow
import social.waddle.android.feature.dm.DmListViewModel
import social.waddle.android.feature.home.HomeViewModel
import social.waddle.android.feature.home.LiveHuddleCard
import social.waddle.android.feature.home.homeSectionsOf
import social.waddle.android.feature.rooms.RoomActivity
import social.waddle.android.feature.rooms.rememberHuddleJoiner
import social.waddle.android.feature.you.YouIdentity
import social.waddle.android.feature.you.connectionLabelOf
import social.waddle.android.feature.you.presenceOf
import social.waddle.android.jid.bareJidOf
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.LocalOpenConversation
import social.waddle.android.ui.UnreadCountBadge
import social.waddle.android.ui.huddle.AvatarSize
import social.waddle.android.ui.huddle.HuddleAvatar
import social.waddle.android.ui.huddle.LiveDot

/** The pages the sidebar opens; [current] marks the one shown beside it on wide layouts. */
enum class SidebarPage { PEOPLE, EVENTS }

/** What the sidebar lists. */
internal data class SidebarState(
    /** The community the account is in: its server's domain. */
    val community: String,
    val identity: YouIdentity,
    /** Joined rooms, in the order to show. */
    val rooms: List<RoomActivity>,
    val dms: List<DmListItem>,
    /** Live huddles, shown as cards on top of the phone's Home tab. */
    val live: List<RoomActivity> = emptyList(),
)

/** Where the sidebar sends the user. */
class SidebarActions(
    val onOpenPage: (SidebarPage) -> Unit = {},
    val onBrowseRooms: () -> Unit = {},
    val onOpenAllDms: () -> Unit = {},
    val onOpenRoom: (roomJid: String, name: String) -> Unit = { _, _ -> },
    val onOpenDm: (peerJid: String, name: String) -> Unit = { _, _ -> },
    val onOpenGroupDm: (roomJid: String, name: String) -> Unit = { _, _ -> },
    val onOpenYou: () -> Unit = {},
    val onOpenSettings: () -> Unit = {},
    /** Joins a live huddle from its card (phone Home tab). */
    val onJoinHuddle: (RoomActivity) -> Unit = {},
    /** Opens the account switcher (the community header). */
    val onOpenAccounts: (() -> Unit)? = null,
)

/**
 * Chat's sidebar: the community, a filter over everything listed,
 * People, the rooms and the direct messages, and you at the bottom.
 * [tab]: the phone's Home tab, where the tab bar has People and You, so
 * those give way to live huddle cards on top.
 */
@Composable
fun SidebarScreen(actions: SidebarActions, current: SidebarPage? = null, tab: Boolean = false) {
    val graph = LocalAppGraph.current
    val home: HomeViewModel = viewModel(factory = HomeViewModel.factory(graph))
    val dmList: DmListViewModel = viewModel(factory = DmListViewModel.factory(graph))
    val homeState by home.uiState.collectAsStateWithLifecycle()
    val dms by dmList.items.collectAsStateWithLifecycle()
    val session by graph.currentSession.collectAsStateWithLifecycle()
    val vcard by graph.sessionManager.profileStore.selfVcard.collectAsStateWithLifecycle()
    val connection by graph.sessionManager.connectionState.collectAsStateWithLifecycle()
    var showAccounts by rememberSaveable { mutableStateOf(false) }
    if (showAccounts) AccountSheet(onDismiss = { showAccounts = false })
    val jid = session?.jid?.let(::bareJidOf)
    val name = vcard?.fullName?.takeIf { it.isNotBlank() }
        ?: vcard?.nickname?.takeIf { it.isNotBlank() }
        ?: session?.username
        ?: jid.orEmpty()
    SidebarContent(
        state = SidebarState(
            community = jid?.substringAfter('@').orEmpty(),
            identity = YouIdentity(name = name, jid = jid, connection = connection),
            rooms = sidebarRoomsOf(homeState.rooms),
            dms = dms,
            live = if (tab) homeSectionsOf(homeState.rooms).live else emptyList(),
        ),
        actions = SidebarActions(
            onOpenPage = actions.onOpenPage,
            onBrowseRooms = actions.onBrowseRooms,
            onOpenAllDms = actions.onOpenAllDms,
            onOpenRoom = actions.onOpenRoom,
            onOpenDm = actions.onOpenDm,
            onOpenGroupDm = actions.onOpenGroupDm,
            onOpenYou = actions.onOpenYou,
            onOpenSettings = actions.onOpenSettings,
            onJoinHuddle = rememberHuddleJoiner(onJoined = { room -> actions.onOpenRoom(room.roomJid, room.name) }),
            onOpenAccounts = { showAccounts = true },
        ),
        current = current,
        tab = tab,
    )
}

/** Semantics tags shared with instrumented tests. */
object SidebarTestTags {
    const val SCREEN = "sidebar-screen"
    const val PEOPLE = "sidebar-people"
    const val EVENTS = "sidebar-events"
    const val BROWSE_ROOMS = "sidebar-browse-rooms"
    const val ALL_DMS = "sidebar-all-dms"
    const val YOU = "sidebar-you"
    const val SETTINGS = "sidebar-settings"
    const val ACCOUNTS = "sidebar-accounts"
    const val ROOM_PREFIX = "sidebar-room-"
}

/** Joined rooms by name, as chat's sidebar lists them. */
internal fun sidebarRoomsOf(rooms: List<RoomActivity>): List<RoomActivity> =
    rooms.filter { it.joined }.sortedWith(compareBy(String.CASE_INSENSITIVE_ORDER) { it.name })

/** Stateless sidebar. */
@Composable
internal fun SidebarContent(
    state: SidebarState,
    actions: SidebarActions,
    current: SidebarPage? = null,
    tab: Boolean = false,
) {
    val colors = Waddle.colors
    var query by rememberSaveable { mutableStateOf("") }
    val match = { name: String -> query.isBlank() || name.contains(query.trim(), ignoreCase = true) }
    val open = LocalOpenConversation.current
    Column(
        modifier = Modifier
            .fillMaxSize()
            .testTag(SidebarTestTags.SCREEN)
            .background(colors.bgRail)
            .windowInsetsPadding(WindowInsets.safeDrawing.only(WindowInsetsSides.Top + WindowInsetsSides.Horizontal)),
    ) {
        CommunityHeader(state.community, actions.onOpenAccounts)
        SearchField(query = query, onQueryChange = { query = it })
        LazyColumn(
            modifier = Modifier.weight(1f),
            contentPadding = PaddingValues(
                horizontal = Waddle.space.xs,
                vertical = Waddle.space.xs,
            ),
        ) {
            if (query.isBlank() && tab) {
                items(state.live, key = { "live:" + it.roomJid }) { room ->
                    LiveHuddleCard(
                        room = room,
                        onOpen = { actions.onOpenRoom(room.roomJid, room.name) },
                        onJoin = { actions.onJoinHuddle(room) },
                        modifier = Modifier.padding(Waddle.space.xs),
                    )
                }
            } else if (query.isBlank()) {
                item {
                    NavRow(
                        icon = Icons.Outlined.People,
                        label = stringResource(R.string.tab_people),
                        current = current == SidebarPage.PEOPLE,
                        modifier = Modifier.testTag(SidebarTestTags.PEOPLE),
                        onClick = { actions.onOpenPage(SidebarPage.PEOPLE) },
                    )
                }
            }
            if (query.isBlank()) {
                item {
                    NavRow(
                        icon = Icons.Outlined.Event,
                        label = stringResource(R.string.events_title),
                        current = current == SidebarPage.EVENTS,
                        modifier = Modifier.testTag(SidebarTestTags.EVENTS),
                        onClick = { actions.onOpenPage(SidebarPage.EVENTS) },
                    )
                }
            }
            roomSection(state.rooms.filter { match(it.name) }, open, actions)
            dmSection(state.dms.filter { match(nameOf(it.row)) }, open, actions)
        }
        if (!tab) {
            HorizontalDivider(color = colors.borderSoft)
            Footer(identity = state.identity, onOpenYou = actions.onOpenYou, onOpenSettings = actions.onOpenSettings)
        }
    }
}

private fun LazyListScope.roomSection(rooms: List<RoomActivity>, open: String?, actions: SidebarActions) {
    item {
        SectionHeader(text = stringResource(R.string.tab_rooms)) {
            IconButton(onClick = actions.onBrowseRooms, modifier = Modifier.testTag(SidebarTestTags.BROWSE_ROOMS)) {
                Icon(
                    Icons.Outlined.Explore,
                    contentDescription = stringResource(R.string.sidebar_browse_rooms),
                    tint = Waddle.colors.fgMuted,
                )
            }
        }
    }
    items(rooms, key = { "room:" + it.roomJid }) { room ->
        NavRow(
            icon = if (room.forum) Icons.AutoMirrored.Outlined.Chat else Icons.Outlined.Tag,
            label = room.name,
            current = room.roomJid == open,
            modifier = Modifier.testTag(SidebarTestTags.ROOM_PREFIX + room.roomJid),
            unread = room.loudUnread + room.threadUnread > 0 || room.mentions > 0,
            onClick = { actions.onOpenRoom(room.roomJid, room.name) },
        ) {
            if (room.live) LiveDot()
            RowCount(count = if (room.mentions > 0) room.mentions else room.loudUnread, forYou = room.mentions > 0)
        }
    }
}

private fun LazyListScope.dmSection(dms: List<DmListItem>, open: String?, actions: SidebarActions) {
    item {
        SectionHeader(text = stringResource(R.string.dm_list_title)) {
            IconButton(onClick = actions.onOpenAllDms, modifier = Modifier.testTag(SidebarTestTags.ALL_DMS)) {
                Icon(
                    Icons.AutoMirrored.Outlined.List,
                    contentDescription = stringResource(R.string.sidebar_all_dms),
                    tint = Waddle.colors.fgMuted,
                )
            }
        }
    }
    items(dms, key = { "dm:" + jidOf(it.row) }) { item ->
        val row = item.row
        val unread = if (row.isMuted) 0 else row.unreadCount
        when (row) {
            is DmListRow.Peer -> NavRow(
                label = row.name,
                current = row.peerJid == open,
                unread = unread > 0,
                leading = {
                    HuddleAvatar(
                        jid = row.peerJid,
                        name = row.name,
                        size = AvatarSize.XS,
                        presence = item.presence,
                        inHuddle = item.inHuddle,
                        ground = Waddle.colors.bgRail,
                    )
                },
                onClick = { actions.onOpenDm(row.peerJid, row.name) },
            ) { RowCount(count = unread, forYou = false) }
            is DmListRow.Group -> NavRow(
                icon = Icons.Outlined.Groups,
                label = row.name,
                current = row.roomJid == open,
                unread = unread > 0,
                onClick = { actions.onOpenGroupDm(row.roomJid, row.name) },
            ) {
                val mentions = item.mentionCount
                RowCount(count = if (mentions > 0) mentions else unread, forYou = mentions > 0)
            }
        }
    }
}

private fun nameOf(row: DmListRow): String = when (row) {
    is DmListRow.Peer -> row.name
    is DmListRow.Group -> row.name
}

private fun jidOf(row: DmListRow): String = when (row) {
    is DmListRow.Peer -> row.peerJid
    is DmListRow.Group -> row.roomJid
}

/** The community: the waddle mark on a white tile and the server's name. */
@Composable
private fun CommunityHeader(community: String, onOpenAccounts: (() -> Unit)?) {
    val switchLabel = stringResource(R.string.accounts_switch_description)
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = Modifier
            .fillMaxWidth()
            .then(
                if (onOpenAccounts != null) {
                    Modifier
                        .testTag(SidebarTestTags.ACCOUNTS)
                        .clickable(onClickLabel = switchLabel, onClick = onOpenAccounts)
                } else {
                    Modifier
                },
            )
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.sm),
    ) {
        Box(
            contentAlignment = Alignment.Center,
            modifier = Modifier
                .size(MARK_TILE)
                .clip(RoundedCornerShape(Waddle.radius.control))
                .background(Color.White),
        ) {
            Image(
                painter = painterResource(R.drawable.waddle_mark),
                contentDescription = null,
                modifier = Modifier.size(MARK),
            )
        }
        Text(
            text = community.ifBlank { stringResource(R.string.huddle_wordmark) },
            style = Waddle.type.title,
            color = Waddle.colors.fg,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f, fill = false).semantics { heading() },
        )
        if (onOpenAccounts != null) {
            Icon(Icons.Outlined.UnfoldMore, contentDescription = null, tint = Waddle.colors.fgMuted)
        }
    }
}

/** Chat's "Search or jump to…": filters the rooms and people listed below. */
@Composable
private fun SearchField(query: String, onQueryChange: (String) -> Unit) {
    val colors = Waddle.colors
    val shape = RoundedCornerShape(Waddle.radius.control)
    BasicTextField(
        value = query,
        onValueChange = onQueryChange,
        singleLine = true,
        textStyle = Waddle.type.bodySmall.copy(color = colors.fg),
        cursorBrush = SolidColor(colors.focus),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Waddle.space.sm)
            .heightIn(min = Waddle.control.touch)
            .clip(shape)
            .background(colors.surface)
            .border(1.dp, colors.borderStrong, shape),
        decorationBox = { field ->
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
                modifier = Modifier.padding(horizontal = Waddle.space.sm),
            ) {
                Icon(
                    Icons.Outlined.Search,
                    contentDescription = null,
                    tint = colors.fgMuted,
                    modifier = Modifier.size(ICON),
                )
                Box(Modifier.weight(1f)) {
                    if (query.isEmpty()) {
                        Text(
                            stringResource(R.string.sidebar_search_hint),
                            style = Waddle.type.bodySmall,
                            color = colors.fgSoft,
                        )
                    }
                    field()
                }
            }
        },
    )
}

@Composable
private fun SectionHeader(text: String, action: @Composable () -> Unit = {}) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = Waddle.control.touch)
            .padding(start = Waddle.space.sm, top = Waddle.space.sm),
    ) {
        Text(
            text = text,
            style = Waddle.type.eyebrow,
            color = Waddle.colors.fgMuted,
            modifier = Modifier
                .weight(1f)
                .semantics { heading() },
        )
        action()
    }
}

/**
 * One sidebar row (web `wd-room`): icon or avatar, name, then trailing
 * marks. Unread rows are bold and bright; the current row gets the
 * selected tint and a bar at the left edge.
 */
@Composable
private fun NavRow(
    label: String,
    current: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    icon: ImageVector? = null,
    unread: Boolean = false,
    leading: (@Composable () -> Unit)? = null,
    trailing: @Composable RowScope.() -> Unit = {},
) {
    val colors = Waddle.colors
    val shape = RoundedCornerShape(Waddle.radius.control)
    val emphasis = current || unread
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(ROW_GAP),
        modifier = modifier
            .fillMaxWidth()
            .heightIn(min = Waddle.control.touch)
            .clip(shape)
            .then(if (current) Modifier.background(colors.fg.copy(alpha = SELECTED_ALPHA)).currentBar() else Modifier)
            .clickable(onClick = onClick)
            .semantics { selected = current }
            .padding(horizontal = ROW_PAD),
    ) {
        when {
            leading != null -> Box(Modifier.size(ICON), contentAlignment = Alignment.Center) { leading() }
            icon != null -> Icon(
                icon,
                contentDescription = null,
                tint = if (emphasis) colors.fg else colors.fgSoft,
                modifier = Modifier.size(ICON),
            )
        }
        Text(
            text = label,
            style = Waddle.type.bodySmall,
            fontWeight = if (emphasis) FontWeight.Bold else FontWeight.Medium,
            color = if (emphasis) colors.fg else colors.fgMuted,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        trailing()
    }
}

/** The current row's bar: 3dp at the left edge, inset 8dp top and bottom. */
@Composable
private fun Modifier.currentBar(): Modifier {
    val fg = Waddle.colors.fg
    return drawBehind {
        val inset = BAR_INSET.toPx()
        val width = BAR_WIDTH.toPx()
        drawRoundRect(
            color = fg,
            topLeft = Offset(0f, inset),
            size = Size(width, size.height - 2 * inset),
            cornerRadius = CornerRadius(width, width),
        )
    }
}

/** A row's count: gold with "@" for mentions, quiet otherwise. */
@Composable
private fun RowCount(count: Int, forYou: Boolean) {
    UnreadCountBadge(count = count, emphasized = forYou)
}

/** You: avatar with presence, name and connection, and Settings. */
@Composable
private fun Footer(identity: YouIdentity, onOpenYou: () -> Unit, onOpenSettings: () -> Unit) {
    val colors = Waddle.colors
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .windowInsetsPadding(WindowInsets.safeDrawing.only(WindowInsetsSides.Bottom))
            .padding(horizontal = Waddle.space.xs, vertical = Waddle.space.xs),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
            modifier = Modifier
                .weight(1f)
                .clip(RoundedCornerShape(Waddle.radius.control))
                .clickable(onClick = onOpenYou)
                .testTag(SidebarTestTags.YOU)
                .padding(Waddle.space.xs),
        ) {
            HuddleAvatar(
                jid = identity.jid,
                name = identity.name,
                size = AvatarSize.MD,
                presence = presenceOf(identity.connection),
                ground = colors.bgRail,
            )
            Column(Modifier.weight(1f)) {
                Text(
                    identity.name,
                    style = Waddle.type.author,
                    color = colors.fg,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    stringResource(connectionLabelOf(identity.connection)),
                    style = Waddle.type.time,
                    color = colors.fgMuted,
                    maxLines = 1,
                )
            }
        }
        IconButton(onClick = onOpenSettings, modifier = Modifier.testTag(SidebarTestTags.SETTINGS)) {
            Icon(
                Icons.Outlined.Tune,
                contentDescription = stringResource(R.string.home_settings),
                tint = colors.fgMuted,
            )
        }
    }
}

private val MARK_TILE = 40.dp
private val MARK = 30.dp
private val ICON = 20.dp
private val ROW_GAP = 10.dp
private val ROW_PAD = 10.dp
private val BAR_WIDTH = 3.dp
private val BAR_INSET = 8.dp
private const val SELECTED_ALPHA = 0.10f
