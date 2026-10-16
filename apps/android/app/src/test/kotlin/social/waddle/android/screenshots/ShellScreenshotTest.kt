package social.waddle.android.screenshots

import android.app.Application
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Home
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.ChatBubbleOutline
import androidx.compose.material.icons.outlined.Group
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.outlined.Notifications
import androidx.compose.material.icons.outlined.Person
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.NavigationBarItemDefaults
import androidx.compose.material3.Text
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import social.waddle.android.avatar.PeerAvatar
import social.waddle.android.client.ConnectionState
import social.waddle.android.feature.conversation.MessageComposer
import social.waddle.android.feature.conversation.MessageRowActions
import social.waddle.android.feature.conversation.MessageRowContext
import social.waddle.android.feature.conversation.TimelineList
import social.waddle.android.feature.home.ChannelListBody
import social.waddle.android.feature.home.ChannelListItem
import social.waddle.android.feature.home.ConnectionBanner
import social.waddle.android.feature.home.SpaceSection
import social.waddle.android.ui.ConversationListRow
import social.waddle.android.ui.GlyphTile
import social.waddle.android.ui.WaddleTopBar
import java.util.Locale
import java.util.TimeZone

/** Root tabs and the conversation screen, composed from their real parts. */
@RunWith(AndroidJUnit4::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [35], qualifiers = "w411dp-h891dp-xxhdpi", application = Application::class)
class ShellScreenshotTest {
    @Before
    fun pinLocale() {
        Locale.setDefault(Locale.UK)
        TimeZone.setDefault(TimeZone.getTimeZone("UTC"))
    }

    @Test
    fun home() = captureThemed("home") {
        Column(Modifier.fillMaxSize()) {
            WaddleTopBar(
                title = { Text("Waddle") },
                actions = { IconButton(onClick = {}) { Icon(Icons.Outlined.Add, contentDescription = null) } },
            )
            // Offline, not reconnecting: an indeterminate spinner never lets the capture go idle.
            ConnectionBanner(state = ConnectionState.Offline, onRetry = {})
            ChannelListBody(
                sections = SECTIONS,
                filter = "",
                onFilterChange = {},
                onChannelClick = {},
                modifier = Modifier.weight(1f),
            )
            BottomBar()
        }
    }

    @Test
    fun dms() = captureThemed("dms") {
        Column(Modifier.fillMaxSize()) {
            WaddleTopBar(title = { Text("Direct messages") })
            Column(Modifier.weight(1f)) {
                DM_ROWS.forEach { row ->
                    val name = row.name
                    ConversationListRow(
                        title = name,
                        leading = {
                            if (name.contains(",")) {
                                GlyphTile(Icons.Outlined.Group)
                            } else {
                                PeerAvatar(jid = "$name@waddle.test", displayName = name, size = 40.dp)
                            }
                        },
                        onClick = {},
                        preview = row.preview,
                        time = row.time,
                        unreadCount = row.unread,
                    )
                }
            }
            BottomBar(selected = 1)
        }
    }

    @Test
    fun conversation() = captureThemed("conversation") {
        Column(Modifier.fillMaxSize()) {
            WaddleTopBar(
                title = {
                    Column {
                        Text("#general", style = MaterialTheme.typography.titleMedium)
                        Text(
                            "Storage, infra and launch chatter",
                            style = MaterialTheme.typography.labelMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                },
                navigationIcon = {
                    IconButton(onClick = {}) { Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = null) }
                },
                actions = {
                    IconButton(onClick = {}) { Icon(Icons.Outlined.Search, contentDescription = null) }
                    IconButton(onClick = {}) { Icon(Icons.Outlined.MoreVert, contentDescription = null) }
                },
            )
            TimelineList(
                entries = ScreenshotFixtures.channelEntries(),
                isLoadingOlder = false,
                reachedHistoryStart = false,
                onTopReached = {},
                actions = MessageRowActions(onOpenThread = {}),
                context = MessageRowContext(selfBareJid = ScreenshotFixtures.SELF),
                today = ScreenshotFixtures.TODAY,
                modifier = Modifier.weight(1f),
            )
            MessageComposer(onSend = { _, _ -> }, onAttach = {}, onGif = {}, onSticker = {})
        }
    }

    @androidx.compose.runtime.Composable
    private fun BottomBar(selected: Int = 0) {
        NavigationBar(containerColor = MaterialTheme.colorScheme.surfaceContainerLow) {
            listOf(
                Triple("Home", Icons.Filled.Home, 0),
                Triple("DMs", Icons.Outlined.ChatBubbleOutline, 1),
                Triple("Activity", Icons.Outlined.Notifications, 2),
                Triple("You", Icons.Outlined.Person, 3),
            ).forEach { (label, icon, index) ->
                NavigationBarItem(
                    selected = index == selected,
                    onClick = {},
                    icon = { Icon(icon, contentDescription = null, modifier = Modifier.size(24.dp)) },
                    label = { Text(label) },
                    colors = NavigationBarItemDefaults.colors(
                        selectedIconColor = MaterialTheme.colorScheme.primary,
                        indicatorColor = MaterialTheme.colorScheme.primary.copy(alpha = 0.14f),
                    ),
                )
            }
        }
    }

    private data class DmRow(val name: String, val preview: String, val time: String, val unread: Int)

    private companion object {
        val SECTIONS = listOf(
            SpaceSection(
                id = "eng",
                name = "Engineering",
                channels = listOf(
                    ChannelListItem("general@muc", "general", unreadCount = 3),
                    ChannelListItem("storage@muc", "storage", unreadCount = 0, inCallCount = 2),
                    ChannelListItem("releases@muc", "releases", unreadCount = 12),
                    ChannelListItem("random@muc", "random", unreadCount = 0, isMuted = true),
                ),
            ),
            SpaceSection(
                id = "community",
                name = "Community",
                channels = listOf(
                    ChannelListItem("welcome@muc", "welcome", unreadCount = 0),
                    ChannelListItem("showcase@muc", "showcase", unreadCount = 1),
                ),
            ),
        )
        val DM_ROWS = listOf(
            DmRow("alice", "Pushed the Parquet compaction fix", "09:41", 2),
            DmRow("bob", "Sounds good, let's ship it Monday", "Wed", 0),
            DmRow("alice, bob, carol", "carol: dashboards are up", "Tue", 0),
            DmRow("dana", "thanks!", "12 Oct", 0),
        )
    }
}
