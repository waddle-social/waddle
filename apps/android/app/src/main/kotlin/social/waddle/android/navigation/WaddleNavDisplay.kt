package social.waddle.android.navigation

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.Chat
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.adaptive.ExperimentalMaterial3AdaptiveApi
import androidx.compose.material3.adaptive.currentWindowAdaptiveInfo
import androidx.compose.material3.adaptive.layout.calculatePaneScaffoldDirective
import androidx.compose.material3.adaptive.navigation3.ListDetailSceneStrategy
import androidx.compose.material3.adaptive.navigation3.rememberListDetailSceneStrategy
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.navigation3.rememberViewModelStoreNavEntryDecorator
import androidx.navigation3.runtime.NavBackStack
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider
import androidx.navigation3.runtime.rememberNavBackStack
import androidx.navigation3.runtime.rememberSaveableStateHolderNavEntryDecorator
import androidx.navigation3.ui.NavDisplay
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.feature.admin.CommunityUsersScreen
import social.waddle.android.feature.channel.ChannelScreen
import social.waddle.android.feature.conversation.ThreadScreen
import social.waddle.android.feature.dm.DmListScreen
import social.waddle.android.feature.dm.DmScreen
import social.waddle.android.feature.dm.GroupDmScreen
import social.waddle.android.feature.events.EventsScreen
import social.waddle.android.feature.home.HomeViewModel
import social.waddle.android.feature.members.MembersScreen
import social.waddle.android.feature.people.PeopleScreen
import social.waddle.android.feature.profile.ProfileScreen
import social.waddle.android.feature.rooms.RoomsScreen
import social.waddle.android.feature.settings.SettingsScreen
import social.waddle.android.feature.sidebar.SidebarActions
import social.waddle.android.feature.sidebar.SidebarPage
import social.waddle.android.feature.sidebar.SidebarScreen
import social.waddle.android.feature.you.YouScreen
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
    // Folded and phones: a bottom tab bar (Home · DMs · People · You),
    // each tab its own back stack, as Slack, Discord and Google Chat do.
    // Read from the live window, so folding and unfolding switch over in
    // place.
    val sidebarPane = calculatePaneScaffoldDirective(currentWindowAdaptiveInfo()).maxHorizontalPartitions > 1
    // ponytail: other tabs' stacks live in memory only; the shown one is saved with the back stack.
    val tabStacks = remember { mutableStateMapOf<MainTab, List<NavKey>>() }
    LaunchedEffect(sidebarPane) {
        if (sidebarPane) tabStacks.clear()
        backStack.adaptTo(sidebarPane)
    }
    val tab = MainTab.rootedAt(backStack.first()) ?: MainTab.HOME

    /** A tab tap: another tab swaps in its stack; the shown one pops to its root. */
    fun select(next: MainTab) {
        if (next == tab) {
            backStack.popToRoot()
        } else {
            tabStacks[tab] = backStack.toList()
            backStack.replaceWith(tabStacks.remove(next) ?: listOf(next.root))
        }
    }
    // Back from another tab's root goes Home before leaving the app.
    BackHandler(enabled = !sidebarPane && backStack.size == 1 && tab != MainTab.HOME) { select(MainTab.HOME) }

    /** A sidebar tap: wide layouts replace what is beside it; the phone Home tab pushes. */
    fun fromSidebar(key: WaddleNavKey) {
        if (sidebarPane) backStack.showConversation(key) else backStack.add(key)
    }

    // A tab root has no back arrow: the tab bar is the way out.
    fun backFrom(root: WaddleNavKey): (() -> Unit)? =
        if (!sidebarPane && backStack.first() == root) null else ({ backStack.popSafely() })
    val sidebarActions = SidebarActions(
        onOpenPage = { page ->
            fromSidebar(
                when (page) {
                    SidebarPage.PEOPLE -> WaddleNavKey.People
                    SidebarPage.EVENTS -> WaddleNavKey.Events
                },
            )
        },
        onBrowseRooms = { fromSidebar(WaddleNavKey.Rooms) },
        onOpenAllDms = { fromSidebar(WaddleNavKey.DmList) },
        onOpenRoom = { roomJid, name -> fromSidebar(WaddleNavKey.Channel(roomJid = roomJid, name = name)) },
        onOpenDm = { peerJid, name -> fromSidebar(WaddleNavKey.Dm(peerJid = peerJid, name = name)) },
        onOpenGroupDm = { roomJid, name -> fromSidebar(WaddleNavKey.GroupDm(roomJid = roomJid, name = name)) },
        onOpenYou = { fromSidebar(WaddleNavKey.You) },
        onOpenSettings = { backStack.add(WaddleNavKey.Settings) },
    )
    val sidebar: @Composable () -> Unit = {
        CompositionLocalProvider(LocalOpenConversation provides openConversationJidOf(backStack)) {
            SidebarScreen(
                actions = sidebarActions,
                current = when (backStack.lastOrNull()) {
                    WaddleNavKey.People -> SidebarPage.PEOPLE
                    WaddleNavKey.Events -> SidebarPage.EVENTS
                    else -> null
                },
                tab = !sidebarPane,
            )
        }
    }
    val home: HomeViewModel = viewModel(factory = HomeViewModel.factory(LocalAppGraph.current))
    val homeState by home.uiState.collectAsStateWithLifecycle()
    Scaffold(
        // Only the bar's height: each screen handles its own insets.
        contentWindowInsets = WindowInsets(0, 0, 0, 0),
        bottomBar = {
            // Hidden only while a conversation is open, as Slack and Discord do.
            if (!sidebarPane && openConversationJidOf(backStack) == null) {
                MainTabBar(
                    selected = tab,
                    badges = TabBadges(dmUnread = homeState.dmUnreadCount, live = homeState.anyHuddleLive),
                    onSelect = ::select,
                )
            }
        },
    ) { padding ->
        NavDisplay(
            modifier = Modifier
                .padding(bottom = padding.calculateBottomPadding())
                .consumeWindowInsets(padding),
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
                    sidebar()
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
                        onBack = backFrom(WaddleNavKey.People),
                    )
                }
                entry<WaddleNavKey.You>(metadata = ListDetailSceneStrategy.detailPane()) {
                    YouScreen(
                        onOpenProfile = { backStack.add(WaddleNavKey.Profile) },
                        onOpenSettings = { backStack.add(WaddleNavKey.Settings) },
                        onBack = backFrom(WaddleNavKey.You),
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
                        onBack = backFrom(WaddleNavKey.DmList),
                    )
                }
                entry<WaddleNavKey.Settings> {
                    SettingsScreen(
                        onBack = { backStack.popSafely() },
                        onOpenCommunityUsers = { backStack.add(WaddleNavKey.CommunityUsers) },
                        onOpenProfile = { backStack.add(WaddleNavKey.Profile) },
                    )
                }
                entry<WaddleNavKey.Events>(metadata = ListDetailSceneStrategy.detailPane()) {
                    EventsScreen(
                        onBack = { backStack.popSafely() },
                        onOpenConversation = { jid, name -> backStack.add(WaddleNavKey.Channel(jid, name)) },
                        onOpenMembers = { jid, name -> backStack.add(WaddleNavKey.Members(jid, name)) },
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

/**
 * Reshape the stack for the layout. Wide: Home (the sidebar) at the
 * root, under whatever a tab had open. Phone: a tab root opened from the
 * sidebar becomes its tab's root.
 */
internal fun NavBackStack<NavKey>.adaptTo(sidebarPane: Boolean) {
    if (sidebarPane) {
        if (first() != WaddleNavKey.Home) add(0, WaddleNavKey.Home)
    } else if (size > 1 && first() == WaddleNavKey.Home && MainTab.rootedAt(this[1]) != null) {
        removeAt(0)
    }
}

/** Swap the whole stack in one go (never empty, which crashes NavDisplay). */
private fun NavBackStack<NavKey>.replaceWith(keys: List<NavKey>) {
    val old = size
    addAll(keys)
    repeat(old) { removeAt(0) }
}

/** Back to the tab's root. */
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
