package social.waddle.android.navigation

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.Chat
import androidx.compose.material3.DrawerValue
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalDrawerSheet
import androidx.compose.material3.ModalNavigationDrawer
import androidx.compose.material3.adaptive.ExperimentalMaterial3AdaptiveApi
import androidx.compose.material3.adaptive.currentWindowAdaptiveInfo
import androidx.compose.material3.adaptive.layout.calculatePaneScaffoldDirective
import androidx.compose.material3.adaptive.navigation3.ListDetailSceneStrategy
import androidx.compose.material3.adaptive.navigation3.rememberListDetailSceneStrategy
import androidx.compose.material3.rememberDrawerState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.navigation3.rememberViewModelStoreNavEntryDecorator
import androidx.navigation3.runtime.NavBackStack
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider
import androidx.navigation3.runtime.rememberNavBackStack
import androidx.navigation3.runtime.rememberSaveableStateHolderNavEntryDecorator
import androidx.navigation3.ui.NavDisplay
import kotlinx.coroutines.launch
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.feature.admin.CommunityUsersScreen
import social.waddle.android.feature.channel.ChannelScreen
import social.waddle.android.feature.conversation.ThreadScreen
import social.waddle.android.feature.dm.DmListScreen
import social.waddle.android.feature.dm.DmScreen
import social.waddle.android.feature.dm.GroupDmScreen
import social.waddle.android.feature.members.MembersScreen
import social.waddle.android.feature.people.PeopleScreen
import social.waddle.android.feature.profile.ProfileScreen
import social.waddle.android.feature.rooms.RoomsScreen
import social.waddle.android.feature.settings.SettingsScreen
import social.waddle.android.feature.sidebar.SidebarActions
import social.waddle.android.feature.sidebar.SidebarPage
import social.waddle.android.feature.sidebar.SidebarScreen
import social.waddle.android.feature.you.YouScreen
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.EmptyState
import social.waddle.android.ui.LocalOpenConversation

/**
 * Navigation 3 host: a saveable [NavBackStack] rooted at the room list rendered by
 * [NavDisplay]. [sideEffects] lets the shell attach back-stack effects
 * (notification-tap navigation) without owning the stack.
 */
@OptIn(ExperimentalMaterial3AdaptiveApi::class)
@Composable
fun WaddleNavDisplay(sideEffects: @Composable (NavBackStack<NavKey>) -> Unit) {
    val backStack = rememberNavBackStack(WaddleNavKey.Home)
    sideEffects(backStack)
    // Unfolded, tablets, landscape: the sidebar is a permanent pane.
    // Folded and phones: rooms are the root and the sidebar a ☰ drawer, as
    // chat's mobile layout has it. Read from the live window, so folding
    // and unfolding switch over in place.
    val sidebarPane = calculatePaneScaffoldDirective(currentWindowAdaptiveInfo()).maxHorizontalPartitions > 1
    val drawer = rememberDrawerState(DrawerValue.Closed)
    val scope = rememberCoroutineScope()
    LaunchedEffect(sidebarPane) {
        if (sidebarPane) drawer.snapTo(DrawerValue.Closed)
    }
    BackHandler(enabled = drawer.isOpen) { scope.launch { drawer.close() } }

    /** A sidebar tap replaces the detail and shuts the drawer; rooms are the phone root. */
    fun fromSidebar(key: WaddleNavKey) {
        if (!sidebarPane && key == WaddleNavKey.Rooms) backStack.popToRoot() else backStack.showConversation(key)
        scope.launch { drawer.close() }
    }
    val sidebarActions = SidebarActions(
        onOpenPage = { page ->
            fromSidebar(
                when (page) {
                    SidebarPage.PEOPLE -> WaddleNavKey.People
                },
            )
        },
        onBrowseRooms = { fromSidebar(WaddleNavKey.Rooms) },
        onOpenAllDms = { fromSidebar(WaddleNavKey.DmList) },
        onOpenRoom = { roomJid, name -> fromSidebar(WaddleNavKey.Channel(roomJid = roomJid, name = name)) },
        onOpenDm = { peerJid, name -> fromSidebar(WaddleNavKey.Dm(peerJid = peerJid, name = name)) },
        onOpenGroupDm = { roomJid, name -> fromSidebar(WaddleNavKey.GroupDm(roomJid = roomJid, name = name)) },
        onOpenYou = { fromSidebar(WaddleNavKey.You) },
        onOpenSettings = {
            backStack.add(WaddleNavKey.Settings)
            scope.launch { drawer.close() }
        },
    )
    val sidebar: @Composable () -> Unit = {
        CompositionLocalProvider(LocalOpenConversation provides openConversationJidOf(backStack)) {
            SidebarScreen(
                actions = sidebarActions,
                current = when (backStack.lastOrNull()) {
                    WaddleNavKey.People -> SidebarPage.PEOPLE
                    else -> null
                },
            )
        }
    }
    ModalNavigationDrawer(
        drawerState = drawer,
        // Swipe in only where the ☰ is: the root.
        gesturesEnabled = !sidebarPane && (drawer.isOpen || backStack.size == 1),
        drawerContent = {
            if (!sidebarPane) {
                ModalDrawerSheet(drawerContainerColor = Waddle.colors.bgRail) { sidebar() }
            }
        },
    ) {
        NavDisplay(
            backStack = backStack,
            onBack = { backStack.popSafely() },
            // Phones: one pane. Unfolded, tablets and landscape: the sidebar
            // as the list pane beside the open conversation, a thread as the
            // extra pane beside that.
            sceneStrategies = listOf(rememberListDetailSceneStrategy()),
            // Each entry owns its ViewModels: popping a conversation clears
            // its ViewModel (and its collectors, joins and refetches) instead
            // of leaking it into the Activity's store for the process lifetime.
            entryDecorators = listOf(
                rememberSaveableStateHolderNavEntryDecorator(),
                rememberViewModelStoreNavEntryDecorator(),
            ),
            entryProvider = entryProvider {
                entry<WaddleNavKey.Home>(
                    metadata = ListDetailSceneStrategy.listPane(detailPlaceholder = { NoConversationOpen() }),
                ) {
                    if (sidebarPane) {
                        sidebar()
                    } else {
                        val speaking by LocalAppGraph.current.callSessionController.speakingJids
                            .collectAsStateWithLifecycle()
                        RoomsScreen(
                            onOpenChannel = { roomJid, name -> backStack.add(WaddleNavKey.Channel(roomJid, name)) },
                            speaking = speaking,
                            onOpenMenu = { scope.launch { drawer.open() } },
                        )
                    }
                }
                // Pages opened from the sidebar; what they open is pushed on
                // top, so back returns to the page.
                entry<WaddleNavKey.Rooms>(metadata = ListDetailSceneStrategy.detailPane()) {
                    val speaking by LocalAppGraph.current.callSessionController.speakingJids
                        .collectAsStateWithLifecycle()
                    RoomsScreen(
                        onOpenChannel = { roomJid, name -> backStack.add(WaddleNavKey.Channel(roomJid, name)) },
                        speaking = speaking,
                        onBack = { backStack.popSafely() },
                    )
                }
                entry<WaddleNavKey.People>(metadata = ListDetailSceneStrategy.detailPane()) {
                    val speaking by LocalAppGraph.current.callSessionController.speakingJids
                        .collectAsStateWithLifecycle()
                    PeopleScreen(
                        onOpenDm = { peerJid, name -> backStack.add(WaddleNavKey.Dm(peerJid, name)) },
                        speaking = speaking,
                        onBack = { backStack.popSafely() },
                    )
                }
                entry<WaddleNavKey.You>(metadata = ListDetailSceneStrategy.detailPane()) {
                    YouScreen(
                        onOpenProfile = { backStack.add(WaddleNavKey.Profile) },
                        onOpenSettings = { backStack.add(WaddleNavKey.Settings) },
                        onBack = { backStack.popSafely() },
                    )
                }
                entry<WaddleNavKey.Channel>(metadata = ListDetailSceneStrategy.detailPane()) { key ->
                    ChannelScreen(
                        roomJid = key.roomJid,
                        name = key.name,
                        onBack = { backStack.popSafely() },
                        onOpenThread = { threadId ->
                            backStack.add(
                                WaddleNavKey.Thread(
                                    conversationJid = key.roomJid,
                                    isGroupchat = true,
                                    threadId = threadId,
                                ),
                            )
                        },
                        onOpenMembers = {
                            backStack.add(WaddleNavKey.Members(roomJid = key.roomJid, name = key.name))
                        },
                        // Pushed on top, so back returns to the conversation.
                        onOpenDm = { peerJid, name -> backStack.add(WaddleNavKey.Dm(peerJid = peerJid, name = name)) },
                    )
                }
                entry<WaddleNavKey.Members> { key ->
                    MembersScreen(
                        roomJid = key.roomJid,
                        name = key.name,
                        onBack = { backStack.popSafely() },
                        // Pushed on top, so back returns to the member list.
                        onOpenDm = { peerJid, name ->
                            backStack.add(WaddleNavKey.Dm(peerJid = peerJid, name = name))
                        },
                    )
                }
                entry<WaddleNavKey.CommunityUsers> {
                    CommunityUsersScreen(onBack = { backStack.popSafely() })
                }
                entry<WaddleNavKey.Dm>(metadata = ListDetailSceneStrategy.detailPane()) { key ->
                    DmScreen(
                        peerJid = key.peerJid,
                        name = key.name,
                        onBack = { backStack.popSafely() },
                        onOpenThread = { threadId ->
                            backStack.add(
                                WaddleNavKey.Thread(
                                    conversationJid = key.peerJid,
                                    isGroupchat = false,
                                    threadId = threadId,
                                ),
                            )
                        },
                        onOpenGroupDm = { roomJid, name ->
                            backStack.add(WaddleNavKey.GroupDm(roomJid = roomJid, name = name))
                        },
                        // Pushed on top, so back returns to the conversation.
                        onOpenDm = { peerJid, name -> backStack.add(WaddleNavKey.Dm(peerJid = peerJid, name = name)) },
                    )
                }
                entry<WaddleNavKey.GroupDm>(metadata = ListDetailSceneStrategy.detailPane()) { key ->
                    GroupDmScreen(
                        roomJid = key.roomJid,
                        name = key.name,
                        onBack = { backStack.popSafely() },
                        onOpenThread = { threadId ->
                            backStack.add(
                                WaddleNavKey.Thread(
                                    conversationJid = key.roomJid,
                                    isGroupchat = true,
                                    threadId = threadId,
                                ),
                            )
                        },
                        onOpenMembers = {
                            backStack.add(WaddleNavKey.Members(roomJid = key.roomJid, name = key.name))
                        },
                        // Pushed on top, so back returns to the conversation.
                        onOpenDm = { peerJid, name -> backStack.add(WaddleNavKey.Dm(peerJid = peerJid, name = name)) },
                    )
                }
                entry<WaddleNavKey.Thread>(metadata = ListDetailSceneStrategy.extraPane()) { key ->
                    ThreadScreen(
                        conversationJid = key.conversationJid,
                        isGroupchat = key.isGroupchat,
                        threadId = key.threadId,
                        onBack = { backStack.popSafely() },
                        // Pushed on top, so back returns to the conversation.
                        onOpenDm = { peerJid, name -> backStack.add(WaddleNavKey.Dm(peerJid = peerJid, name = name)) },
                    )
                }
                entry<WaddleNavKey.DmList>(metadata = ListDetailSceneStrategy.detailPane()) {
                    DmListScreen(
                        onOpenDm = { peerJid, name ->
                            backStack.add(WaddleNavKey.Dm(peerJid = peerJid, name = name))
                        },
                        onOpenGroupDm = { roomJid, name ->
                            backStack.add(WaddleNavKey.GroupDm(roomJid = roomJid, name = name))
                        },
                        onBack = { backStack.popSafely() },
                    )
                }
                entry<WaddleNavKey.Settings> {
                    SettingsScreen(
                        onBack = { backStack.popSafely() },
                        onOpenCommunityUsers = { backStack.add(WaddleNavKey.CommunityUsers) },
                        onOpenProfile = { backStack.add(WaddleNavKey.Profile) },
                    )
                }
                entry<WaddleNavKey.Profile> {
                    ProfileScreen(onBack = { backStack.popSafely() })
                }
            },
        )
    }
}

/** The detail pane before anything is open (wide layouts only). */
@Composable
private fun NoConversationOpen() {
    EmptyState(
        icon = Icons.AutoMirrored.Outlined.Chat,
        title = stringResource(R.string.no_conversation_title),
        body = stringResource(R.string.no_conversation_body),
        modifier = Modifier.background(MaterialTheme.colorScheme.background),
    )
}

/**
 * Open a conversation or page from the sidebar. Wide layouts show the
 * sidebar beside what is open, so a tap there replaces it rather than
 * stacking behind it; on phones the sidebar is only visible at the root,
 * where this is a plain push.
 */
internal fun NavBackStack<NavKey>.showConversation(key: WaddleNavKey) {
    while (size > 1) {
        removeAt(lastIndex)
    }
    add(key)
}

/** Bare JID of the conversation on top of [backStack], if one is. */
internal fun openConversationJidOf(backStack: List<NavKey>): String? =
    when (val top = backStack.lastOrNull()) {
        is WaddleNavKey.Channel -> top.roomJid
        is WaddleNavKey.Dm -> top.peerJid
        is WaddleNavKey.GroupDm -> top.roomJid
        is WaddleNavKey.Thread -> top.conversationJid
        else -> null
    }

/** Back to the room list on phones. */
private fun NavBackStack<NavKey>.popToRoot() {
    while (size > 1) {
        removeAt(lastIndex)
    }
}

/** Pop unless already at the root (an empty back stack crashes NavDisplay). */
private fun NavBackStack<NavKey>.popSafely() {
    if (size > 1) {
        removeAt(lastIndex)
    }
}
