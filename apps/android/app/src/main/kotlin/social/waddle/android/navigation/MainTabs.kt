package social.waddle.android.navigation

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ChatBubble
import androidx.compose.material.icons.filled.Home
import androidx.compose.material.icons.filled.Notifications
import androidx.compose.material.icons.filled.Person
import androidx.compose.material.icons.outlined.ChatBubbleOutline
import androidx.compose.material.icons.outlined.Home
import androidx.compose.material.icons.outlined.Notifications
import androidx.compose.material.icons.outlined.Person
import androidx.compose.material3.Badge
import androidx.compose.material3.BadgedBox
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.NavigationBarItemDefaults
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.feature.activity.ActivityScreen
import social.waddle.android.feature.dm.DmListScreen
import social.waddle.android.feature.home.HomeScreen
import social.waddle.android.feature.home.HomeViewModel
import social.waddle.android.feature.you.YouScreen
import social.waddle.android.theme.Waddle

/** The four root destinations (iOS PhoneShell parity). */
enum class MainTab(
    val label: Int,
    val icon: ImageVector,
    val selectedIcon: ImageVector,
    val testTag: String,
) {
    HOME(R.string.tab_home, Icons.Outlined.Home, Icons.Filled.Home, "tab-home"),
    DMS(R.string.tab_dms, Icons.Outlined.ChatBubbleOutline, Icons.Filled.ChatBubble, "tab-dms"),
    ACTIVITY(R.string.tab_activity, Icons.Outlined.Notifications, Icons.Filled.Notifications, "tab-activity"),
    YOU(R.string.tab_you, Icons.Outlined.Person, Icons.Filled.Person, "tab-you"),
}

/** Where a root tab can send the user; the back stack owns the rest. */
class MainTabsActions(
    val openChannel: (roomJid: String, name: String) -> Unit,
    val openDm: (peerJid: String, name: String) -> Unit,
    val openGroupDm: (roomJid: String, name: String) -> Unit,
    val openConversation: (jid: String) -> Unit,
    val openProfile: () -> Unit,
    val openSettings: () -> Unit,
)

/**
 * Root shell: Home · DMs · Activity · You on a bottom bar. Tabs keep
 * their own state; conversations open full screen over the bar.
 */
@Composable
fun MainTabs(actions: MainTabsActions) {
    val graph = LocalAppGraph.current
    val homeViewModel: HomeViewModel = viewModel(factory = HomeViewModel.factory(graph))
    val home by homeViewModel.uiState.collectAsStateWithLifecycle()
    val counts by graph.sessionManager.unreadStore.counts.collectAsStateWithLifecycle()
    var tab by rememberSaveable { mutableStateOf(MainTab.HOME) }
    val haptics = LocalHapticFeedback.current
    val channelUnread = home.sections.sumOf { section ->
        section.channels.filterNot { it.isMuted }.sumOf { it.unreadCount }
    }
    val anyUnread = counts.values.any { it > 0 }
    // The bar draws under the navigation bar; each tab handles the top only.
    val tabInsets = WindowInsets.safeDrawing.only(WindowInsetsSides.Top + WindowInsetsSides.Horizontal)

    Scaffold(
        bottomBar = {
            NavigationBar(containerColor = MaterialTheme.colorScheme.surfaceContainerLow) {
                MainTab.entries.forEach { entry ->
                    val selected = entry == tab
                    NavigationBarItem(
                        selected = selected,
                        onClick = {
                            if (!selected) haptics.performHapticFeedback(HapticFeedbackType.SegmentTick)
                            tab = entry
                        },
                        icon = {
                            TabIcon(
                                icon = if (selected) entry.selectedIcon else entry.icon,
                                count = when (entry) {
                                    MainTab.HOME -> channelUnread
                                    MainTab.DMS -> home.dmUnreadCount
                                    else -> 0
                                },
                                dot = entry == MainTab.ACTIVITY && anyUnread,
                            )
                        },
                        label = { Text(text = stringResource(entry.label)) },
                        colors = NavigationBarItemDefaults.colors(
                            selectedIconColor = MaterialTheme.colorScheme.primary,
                            selectedTextColor = MaterialTheme.colorScheme.onSurface,
                            indicatorColor = MaterialTheme.colorScheme.primary.copy(alpha = 0.14f),
                            unselectedIconColor = MaterialTheme.colorScheme.onSurfaceVariant,
                            unselectedTextColor = MaterialTheme.colorScheme.onSurfaceVariant,
                        ),
                        modifier = Modifier.testTag(entry.testTag),
                    )
                }
            }
        },
    ) { padding ->
        Box(
            modifier = Modifier
                .fillMaxSize()
                .padding(bottom = padding.calculateBottomPadding())
                .consumeWindowInsets(padding),
        ) {
            when (tab) {
                MainTab.HOME -> HomeScreen(onOpenChannel = actions.openChannel, contentWindowInsets = tabInsets)
                MainTab.DMS -> DmListScreen(
                    onOpenDm = actions.openDm,
                    onOpenGroupDm = actions.openGroupDm,
                    contentWindowInsets = tabInsets,
                )
                MainTab.ACTIVITY -> ActivityScreen(
                    onOpenConversation = actions.openConversation,
                    contentWindowInsets = tabInsets,
                )
                MainTab.YOU -> YouScreen(
                    onOpenProfile = actions.openProfile,
                    onOpenSettings = actions.openSettings,
                    contentWindowInsets = tabInsets,
                )
            }
        }
    }
}

/** Tab glyph with a neutral count (or a dot for "something new"). */
@Composable
private fun TabIcon(icon: ImageVector, count: Int, dot: Boolean) {
    BadgedBox(
        badge = {
            when {
                count > 0 -> Badge(
                    containerColor = Waddle.colors.neutralBadge,
                    contentColor = Waddle.colors.onNeutralBadge,
                ) { Text(text = if (count > MAX_TAB_BADGE) "$MAX_TAB_BADGE+" else count.toString()) }
                dot -> Badge(containerColor = MaterialTheme.colorScheme.primary)
            }
        },
    ) {
        Icon(icon, contentDescription = null)
    }
}

private const val MAX_TAB_BADGE = 99
