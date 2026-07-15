package social.waddle.android.navigation

import androidx.compose.runtime.Composable
import androidx.navigation3.runtime.NavBackStack
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider
import androidx.navigation3.runtime.rememberNavBackStack
import androidx.navigation3.ui.NavDisplay
import social.waddle.android.feature.channel.ChannelScreen
import social.waddle.android.feature.dm.DmListScreen
import social.waddle.android.feature.dm.DmScreen
import social.waddle.android.feature.home.HomeScreen
import social.waddle.android.feature.settings.SettingsScreen

/**
 * Navigation 3 host: a saveable [NavBackStack] rooted at Home rendered by
 * [NavDisplay]. [sideEffects] lets the shell attach back-stack effects
 * (notification-tap navigation) without owning the stack.
 */
@Composable
fun WaddleNavDisplay(sideEffects: @Composable (NavBackStack<NavKey>) -> Unit) {
    val backStack = rememberNavBackStack(WaddleNavKey.Home)
    sideEffects(backStack)
    NavDisplay(
        backStack = backStack,
        onBack = { backStack.popSafely() },
        entryProvider = entryProvider {
            entry<WaddleNavKey.Home> {
                HomeScreen(
                    onOpenChannel = { roomJid, name ->
                        backStack.add(WaddleNavKey.Channel(roomJid = roomJid, name = name))
                    },
                    onOpenDmList = { backStack.add(WaddleNavKey.DmList) },
                    onOpenSettings = { backStack.add(WaddleNavKey.Settings) },
                )
            }
            entry<WaddleNavKey.Channel> { key ->
                ChannelScreen(
                    roomJid = key.roomJid,
                    name = key.name,
                    onBack = { backStack.popSafely() },
                )
            }
            entry<WaddleNavKey.Dm> { key ->
                DmScreen(
                    peerJid = key.peerJid,
                    name = key.name,
                    onBack = { backStack.popSafely() },
                )
            }
            entry<WaddleNavKey.DmList> {
                DmListScreen(
                    onOpenDm = { peerJid, name ->
                        backStack.add(WaddleNavKey.Dm(peerJid = peerJid, name = name))
                    },
                    onBack = { backStack.popSafely() },
                )
            }
            entry<WaddleNavKey.Settings> {
                SettingsScreen(onBack = { backStack.popSafely() })
            }
        },
    )
}

/** Pop unless already at the root (an empty back stack crashes NavDisplay). */
private fun NavBackStack<NavKey>.popSafely() {
    if (size > 1) {
        removeAt(lastIndex)
    }
}
