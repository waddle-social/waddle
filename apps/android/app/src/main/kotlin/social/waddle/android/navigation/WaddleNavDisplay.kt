package social.waddle.android.navigation

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.only
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.automirrored.outlined.Chat
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ScaffoldDefaults
import androidx.compose.material3.adaptive.ExperimentalMaterial3AdaptiveApi
import androidx.compose.material3.adaptive.navigation3.ListDetailSceneStrategy
import androidx.compose.material3.adaptive.navigation3.rememberListDetailSceneStrategy
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
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
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.feature.admin.CommunityUsersScreen
import social.waddle.android.feature.channel.ChannelScreen
import social.waddle.android.feature.conversation.ThreadScreen
import social.waddle.android.feature.dm.DmListScreen
import social.waddle.android.feature.dm.DmScreen
import social.waddle.android.feature.dm.GroupDmScreen
import social.waddle.android.feature.home.HomeScreen
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
import social.waddle.android.ui.WaddleTopBar

/**
 * Navigation 3 host: a saveable [NavBackStack] rooted at Home rendered by
 * [NavDisplay]. [sideEffects] lets the shell attach back-stack effects
 * (notification-tap navigation) without owning the stack.
 */
@OptIn(ExperimentalMaterial3AdaptiveApi::class)
@Composable
fun WaddleNavDisplay(sideEffects: @Composable (NavBackStack<NavKey>) -> Unit) {
    val backStack = rememberNavBackStack(WaddleNavKey.Home)
    sideEffects(backStack)
    NavDisplay(
        backStack = backStack,
        onBack = { backStack.popSafely() },
        // Phones: one pane. Tablets, foldables and landscape: the sidebar as
        // the list pane beside the open conversation, a thread as the
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
                CompositionLocalProvider(LocalOpenConversation provides openConversationJidOf(backStack)) {
                    SidebarScreen(
                        // Sidebar taps replace what is open beside it.
                        actions = SidebarActions(
                            onOpenPage = { page ->
                                backStack.showConversation(
                                    when (page) {
                                        SidebarPage.DASHBOARD -> WaddleNavKey.Dashboard
                                        SidebarPage.PEOPLE -> WaddleNavKey.People
                                    },
                                )
                            },
                            onBrowseRooms = { backStack.showConversation(WaddleNavKey.Rooms) },
                            onOpenAllDms = { backStack.showConversation(WaddleNavKey.DmList) },
                            onOpenRoom = { roomJid, name ->
                                backStack.showConversation(WaddleNavKey.Channel(roomJid = roomJid, name = name))
                            },
                            onOpenDm = { peerJid, name ->
                                backStack.showConversation(WaddleNavKey.Dm(peerJid = peerJid, name = name))
                            },
                            onOpenGroupDm = { roomJid, name ->
                                backStack.showConversation(WaddleNavKey.GroupDm(roomJid = roomJid, name = name))
                            },
                            onOpenYou = { backStack.showConversation(WaddleNavKey.You) },
                            onOpenSettings = { backStack.add(WaddleNavKey.Settings) },
                        ),
                        current = when (backStack.lastOrNull()) {
                            WaddleNavKey.Dashboard -> SidebarPage.DASHBOARD
                            WaddleNavKey.People -> SidebarPage.PEOPLE
                            else -> null
                        },
                    )
                }
            }
            // Pages opened from the sidebar; what they open is pushed on
            // top, so back returns to the page.
            entry<WaddleNavKey.Dashboard>(metadata = ListDetailSceneStrategy.detailPane()) {
                val speaking by LocalAppGraph.current.callSessionController.speakingJids.collectAsStateWithLifecycle()
                PushedPage(onBack = { backStack.popSafely() }) { insets ->
                    HomeScreen(
                        onOpenChannel = { roomJid, name -> backStack.add(WaddleNavKey.Channel(roomJid, name)) },
                        contentWindowInsets = insets,
                        onOpenDm = { peerJid, name -> backStack.add(WaddleNavKey.Dm(peerJid, name)) },
                        onOpenYou = { backStack.add(WaddleNavKey.You) },
                        onOpenPeople = { backStack.add(WaddleNavKey.People) },
                        onOpenRooms = { backStack.add(WaddleNavKey.Rooms) },
                        speaking = speaking,
                    )
                }
            }
            entry<WaddleNavKey.Rooms>(metadata = ListDetailSceneStrategy.detailPane()) {
                val speaking by LocalAppGraph.current.callSessionController.speakingJids.collectAsStateWithLifecycle()
                PushedPage(onBack = { backStack.popSafely() }) { insets ->
                    RoomsScreen(
                        onOpenChannel = { roomJid, name -> backStack.add(WaddleNavKey.Channel(roomJid, name)) },
                        contentWindowInsets = insets,
                        speaking = speaking,
                    )
                }
            }
            entry<WaddleNavKey.People>(metadata = ListDetailSceneStrategy.detailPane()) {
                val speaking by LocalAppGraph.current.callSessionController.speakingJids.collectAsStateWithLifecycle()
                PushedPage(onBack = { backStack.popSafely() }) { insets ->
                    PeopleScreen(
                        onOpenDm = { peerJid, name -> backStack.add(WaddleNavKey.Dm(peerJid, name)) },
                        contentWindowInsets = insets,
                        speaking = speaking,
                    )
                }
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

/**
 * A page opened from the sidebar: a back arrow over [content], which
 * gets the insets left once the bar has taken the top.
 */
@Composable
private fun PushedPage(onBack: () -> Unit, content: @Composable (WindowInsets) -> Unit) {
    Column(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background)) {
        WaddleTopBar(
            title = {},
            navigationIcon = {
                IconButton(onClick = onBack) {
                    Icon(
                        Icons.AutoMirrored.Outlined.ArrowBack,
                        contentDescription = stringResource(R.string.action_back),
                    )
                }
            },
        )
        content(ScaffoldDefaults.contentWindowInsets.only(WindowInsetsSides.Bottom + WindowInsetsSides.Horizontal))
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

/** Pop unless already at the root (an empty back stack crashes NavDisplay). */
private fun NavBackStack<NavKey>.popSafely() {
    if (size > 1) {
        removeAt(lastIndex)
    }
}
