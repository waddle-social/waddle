package social.waddle.android.navigation

import androidx.activity.compose.BackHandler
import androidx.compose.animation.Crossfade
import androidx.compose.animation.core.snap
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
import androidx.compose.material.icons.filled.People
import androidx.compose.material.icons.filled.Person
import androidx.compose.material.icons.filled.Tag
import androidx.compose.material.icons.outlined.ChatBubbleOutline
import androidx.compose.material.icons.outlined.Home
import androidx.compose.material.icons.outlined.People
import androidx.compose.material.icons.outlined.Person
import androidx.compose.material.icons.outlined.Tag
import androidx.compose.material3.Scaffold
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.feature.dm.DmListScreen
import social.waddle.android.feature.home.HomeScreen
import social.waddle.android.feature.home.HomeViewModel
import social.waddle.android.feature.people.PeopleScreen
import social.waddle.android.feature.rooms.RoomsScreen
import social.waddle.android.feature.you.YouScreen
import social.waddle.android.ui.huddle.HuddleMotion
import social.waddle.android.ui.huddle.reducedMotion

/** The five root destinations: Home · Rooms · People · DMs · You. */
enum class MainTab(
    val label: Int,
    val icon: ImageVector,
    val selectedIcon: ImageVector,
    val testTag: String,
) {
    HOME(R.string.tab_home, Icons.Outlined.Home, Icons.Filled.Home, "tab-home"),
    ROOMS(R.string.tab_rooms, Icons.Outlined.Tag, Icons.Filled.Tag, "tab-rooms"),
    PEOPLE(R.string.tab_people, Icons.Outlined.People, Icons.Filled.People, "tab-people"),
    DMS(R.string.tab_dms, Icons.Outlined.ChatBubbleOutline, Icons.Filled.ChatBubble, "tab-dms"),
    YOU(R.string.tab_you, Icons.Outlined.Person, Icons.Filled.Person, "tab-you"),
    ;

    companion object {
        /**
         * The tab saved under [name], else Home: the selection is saved by
         * name, so a renamed or removed tab restores to Home instead of
         * crashing state restoration.
         */
        fun fromName(name: String?): MainTab = entries.firstOrNull { it.name == name } ?: HOME
    }
}

/** What the tab bar marks: unread direct messages, and whether a huddle is live. */
data class TabBadges(val dmUnread: Int = 0, val roomsLive: Boolean = false)

/** Where a root tab can send the user; the back stack owns the rest. */
class MainTabsActions(
    val openChannel: (roomJid: String, name: String) -> Unit,
    val openDm: (peerJid: String, name: String) -> Unit,
    val openGroupDm: (roomJid: String, name: String) -> Unit,
    val openProfile: () -> Unit,
    val openSettings: () -> Unit,
)

/**
 * Root shell: Home · Rooms · People · DMs · You on the Huddle tab bar.
 * Conversations open full screen over the bar. Back from any tab but
 * Home returns to Home first ([atRoot]: only while nothing is open over
 * the tabs, so a wide layout's open conversation still pops first).
 */
@Composable
fun MainTabs(actions: MainTabsActions, atRoot: Boolean = true) {
    val graph = LocalAppGraph.current
    val homeViewModel: HomeViewModel = viewModel(factory = HomeViewModel.factory(graph))
    val home by homeViewModel.uiState.collectAsStateWithLifecycle()
    var tabName by rememberSaveable { mutableStateOf(MainTab.HOME.name) }
    val tab = MainTab.fromName(tabName)
    val select = { next: MainTab -> tabName = next.name }
    BackHandler(enabled = atRoot && tab != MainTab.HOME) { select(MainTab.HOME) }
    // The bar draws under the navigation bar; each tab handles the top only.
    val tabInsets = WindowInsets.safeDrawing.only(WindowInsetsSides.Top + WindowInsetsSides.Horizontal)

    Scaffold(
        // No content insets here: the padding below is consumed for the
        // tabs, so a status-bar inset in it would be marked handled without
        // being applied, and each tab's top bar would draw under the
        // status bar.
        contentWindowInsets = WindowInsets(0, 0, 0, 0),
        bottomBar = {
            MainTabBar(
                selected = tab,
                badges = TabBadges(dmUnread = home.dmUnreadCount, roomsLive = home.anyHuddleLive),
                onSelect = select,
            )
        },
    ) { padding ->
        Box(
            modifier = Modifier
                .fillMaxSize()
                .padding(bottom = padding.calculateBottomPadding())
                .consumeWindowInsets(padding),
        ) {
            Crossfade(
                targetState = tab,
                animationSpec = if (reducedMotion) snap() else HuddleMotion.fast(),
                label = "tab",
            ) { shown ->
                TabContent(tab = shown, actions = actions, tabInsets = tabInsets, select = select)
            }
        }
    }
}

@Composable
private fun TabContent(
    tab: MainTab,
    actions: MainTabsActions,
    tabInsets: WindowInsets,
    select: (MainTab) -> Unit,
) {
    // Who is talking in the call this device is in (empty outside one):
    // lights speaking rings on Home, Rooms and People.
    val speaking by LocalAppGraph.current.callSessionController.speakingJids.collectAsStateWithLifecycle()
    when (tab) {
        MainTab.HOME -> HomeScreen(
            onOpenChannel = actions.openChannel,
            contentWindowInsets = tabInsets,
            onOpenDm = actions.openDm,
            onOpenYou = { select(MainTab.YOU) },
            onOpenPeople = { select(MainTab.PEOPLE) },
            onOpenRooms = { select(MainTab.ROOMS) },
            speaking = speaking,
        )
        MainTab.ROOMS -> RoomsScreen(
            onOpenChannel = actions.openChannel,
            contentWindowInsets = tabInsets,
            speaking = speaking,
        )
        MainTab.PEOPLE -> PeopleScreen(
            onOpenDm = actions.openDm,
            contentWindowInsets = tabInsets,
            speaking = speaking,
        )
        MainTab.DMS -> DmListScreen(
            onOpenDm = actions.openDm,
            onOpenGroupDm = actions.openGroupDm,
            contentWindowInsets = tabInsets,
        )
        MainTab.YOU -> YouScreen(
            onOpenProfile = actions.openProfile,
            onOpenSettings = actions.openSettings,
            contentWindowInsets = tabInsets,
        )
    }
}
