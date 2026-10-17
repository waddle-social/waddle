package social.waddle.android.navigation

import androidx.compose.foundation.background
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.Chat
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.adaptive.ExperimentalMaterial3AdaptiveApi
import androidx.compose.material3.adaptive.navigation3.ListDetailSceneStrategy
import androidx.compose.material3.adaptive.navigation3.rememberListDetailSceneStrategy
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.viewmodel.navigation3.rememberViewModelStoreNavEntryDecorator
import androidx.navigation3.runtime.NavBackStack
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider
import androidx.navigation3.runtime.rememberNavBackStack
import androidx.navigation3.runtime.rememberSaveableStateHolderNavEntryDecorator
import androidx.navigation3.ui.NavDisplay
import social.waddle.android.R
import social.waddle.android.feature.admin.CommunityUsersScreen
import social.waddle.android.feature.channel.ChannelScreen
import social.waddle.android.feature.conversation.ThreadScreen
import social.waddle.android.feature.dm.DmListScreen
import social.waddle.android.feature.dm.DmScreen
import social.waddle.android.feature.dm.GroupDmScreen
import social.waddle.android.feature.members.MembersScreen
import social.waddle.android.feature.profile.ProfileScreen
import social.waddle.android.feature.settings.SettingsScreen
import social.waddle.android.ui.EmptyState
import social.waddle.android.ui.LocalOpenConversation

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
        // Phones: one pane. Tablets, foldables and landscape: the tabs as
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
                    MainTabs(
                        actions = MainTabsActions(
                            openChannel = { roomJid, name ->
                                backStack.showConversation(WaddleNavKey.Channel(roomJid = roomJid, name = name))
                            },
                            openDm = { peerJid, name ->
                                backStack.showConversation(WaddleNavKey.Dm(peerJid = peerJid, name = name))
                            },
                            openGroupDm = { roomJid, name ->
                                backStack.showConversation(WaddleNavKey.GroupDm(roomJid = roomJid, name = name))
                            },
                            openProfile = { backStack.add(WaddleNavKey.Profile) },
                            openSettings = { backStack.add(WaddleNavKey.Settings) },
                        ),
                        // Wide layouts show the tabs beside an open
                        // conversation; back then closes that first.
                        atRoot = backStack.size == 1,
                    )
                }
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
            entry<WaddleNavKey.DmList> {
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
 * Open a conversation from the lists. Wide layouts show the lists beside
 * the open conversation, so a tap there replaces it rather than stacking
 * conversations behind each other; on phones the lists are only visible
 * at the root, where this is a plain push.
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
