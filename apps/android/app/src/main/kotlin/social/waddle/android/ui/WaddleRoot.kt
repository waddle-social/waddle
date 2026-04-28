package social.waddle.android.ui

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Forum
import androidx.compose.material.icons.outlined.Notifications
import androidx.compose.material.icons.outlined.Person
import androidx.compose.material.icons.outlined.Tag
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.adaptive.navigationsuite.NavigationSuiteScaffold
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.vector.ImageVector
import social.waddle.android.ui.activity.ActivityScreen
import social.waddle.android.ui.dms.DmListScreen
import social.waddle.android.ui.profile.ProfileScreen
import social.waddle.android.ui.rooms.RoomListScreen

internal enum class TopLevelDestination(
    val title: String,
    val icon: ImageVector,
) {
    Channels("Channels", Icons.Outlined.Tag),
    DirectMessages("DMs", Icons.Outlined.Forum),
    Activity("Activity", Icons.Outlined.Notifications),
    Profile("You", Icons.Outlined.Person),
}

@Composable
internal fun WaddleRoot() {
    var current by remember { mutableStateOf(TopLevelDestination.Channels) }
    NavigationSuiteScaffold(
        navigationSuiteItems = {
            TopLevelDestination.entries.forEach { destination ->
                item(
                    selected = current == destination,
                    onClick = { current = destination },
                    icon = { Icon(destination.icon, contentDescription = destination.title) },
                    label = { Text(destination.title) },
                )
            }
        },
    ) {
        when (current) {
            TopLevelDestination.Channels -> RoomListScreen()
            TopLevelDestination.DirectMessages -> DmListScreen()
            TopLevelDestination.Activity -> ActivityScreen()
            TopLevelDestination.Profile -> ProfileScreen()
        }
    }
}
