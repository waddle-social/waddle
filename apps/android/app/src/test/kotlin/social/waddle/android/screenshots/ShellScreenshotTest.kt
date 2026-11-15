package social.waddle.android.screenshots

import android.app.Application
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Density
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import social.waddle.android.client.ConnectionState
import social.waddle.android.client.store.PresenceShow
import social.waddle.android.feature.dm.DmListItem
import social.waddle.android.feature.dm.DmListRow
import social.waddle.android.feature.home.HomeActions
import social.waddle.android.feature.home.HomeContent
import social.waddle.android.feature.home.HomeContentState
import social.waddle.android.feature.people.PeopleContent
import social.waddle.android.feature.people.PersonRow
import social.waddle.android.feature.rooms.HuddleParticipant
import social.waddle.android.feature.rooms.RoomActivity
import social.waddle.android.feature.rooms.RoomActivityOrder
import social.waddle.android.feature.rooms.RoomsContent
import social.waddle.android.feature.rooms.RoomsUiState
import social.waddle.android.feature.sidebar.SidebarActions
import social.waddle.android.feature.sidebar.SidebarContent
import social.waddle.android.feature.sidebar.SidebarPage
import social.waddle.android.feature.sidebar.SidebarState
import social.waddle.android.feature.sidebar.sidebarRoomsOf
import social.waddle.android.feature.you.YouIdentity
import social.waddle.android.ui.LocalOpenConversation
import social.waddle.android.ui.huddle.LocalReducedMotion
import java.time.ZoneOffset
import java.time.ZonedDateTime
import java.util.Locale
import java.util.TimeZone

/**
 * The shell's sidebar and the pages it opens, rendered from their real
 * stateless content composables over fixed fixtures.
 * Locale, zone, clock and reduced motion are pinned.
 */
@RunWith(AndroidJUnit4::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [35], qualifiers = "en-rGB-w411dp-h891dp-xxhdpi", application = Application::class)
class ShellScreenshotTest {
    @Before
    fun pinLocale() {
        Locale.setDefault(Locale.UK)
        TimeZone.setDefault(TimeZone.getTimeZone("UTC"))
    }

    @Test
    fun homeBusy() = captureThemed("home_busy") {
        Shell {
            HomeContent(state = Fixtures.busyHome, now = Fixtures.AFTERNOON, actions = HomeActions(onOpenMenu = {}))
        }
    }

    @Test
    fun homeQuiet() = captureThemed("home_quiet") {
        Shell {
            HomeContent(state = Fixtures.quietHome, now = Fixtures.EVENING, actions = HomeActions(onOpenMenu = {}))
        }
    }

    @Test
    fun homeLoading() = captureThemed("home_loading") {
        Shell {
            HomeContent(state = Fixtures.loadingHome, now = Fixtures.MORNING, actions = HomeActions(onOpenMenu = {}))
        }
    }

    /** 1.3× text: nothing may clip or collide. */
    @Test
    fun homeBusyLargeText() = captureThemed("home_busy_large_text") {
        val density = LocalDensity.current
        CompositionLocalProvider(LocalDensity provides Density(density.density, fontScale = LARGE_TEXT)) {
            Shell {
                HomeContent(state = Fixtures.busyHome, now = Fixtures.AFTERNOON, actions = HomeActions(onOpenMenu = {}))
            }
        }
    }

    @Test
    fun rooms() = captureThemed("rooms") {
        Shell {
            RoomsContent(
                state = RoomsUiState(rooms = Fixtures.rooms.sortedWith(RoomActivityOrder), settled = true),
                query = "",
                onQueryChange = {},
                nowMillis = Fixtures.AFTERNOON.toInstant().toEpochMilli(),
                canCreate = true,
                onCreate = {},
                onEnter = {},
                onJoin = {},
            )
        }
    }

    @Test
    fun people() = captureThemed("people") {
        Shell {
            PeopleContent(
                people = Fixtures.people,
                settled = true,
                query = "",
                onQueryChange = {},
                everyone = false,
                onEveryoneChange = {},
                onOpen = {},
            )
        }
    }

    /** The start screen: community, People, rooms, direct messages and you. */
    @Test
    fun sidebar() = captureThemed("sidebar") {
        Shell {
            CompositionLocalProvider(LocalOpenConversation provides "general@muc.waddle.test") {
                SidebarContent(state = Fixtures.sidebar, actions = SidebarActions())
            }
        }
    }

    @Test
    fun sidebarLargeText() = captureThemed("sidebar_large_text") {
        val density = LocalDensity.current
        CompositionLocalProvider(LocalDensity provides Density(density.density, fontScale = LARGE_TEXT)) {
            Shell {
                SidebarContent(state = Fixtures.sidebar, actions = SidebarActions(), current = SidebarPage.PEOPLE)
            }
        }
    }

    @Composable
    private fun Shell(content: @Composable () -> Unit) {
        CompositionLocalProvider(LocalReducedMotion provides true) {
            Column(Modifier.fillMaxSize()) { content() }
        }
    }

    private companion object {
        const val LARGE_TEXT = 1.3f
    }
}

/** A small, believable community: the same people and rooms in every shell golden. */
private object Fixtures {
    val AFTERNOON: ZonedDateTime = ZonedDateTime.of(2026, 10, 16, 14, 5, 0, 0, ZoneOffset.UTC)
    val EVENING: ZonedDateTime = ZonedDateTime.of(2026, 10, 16, 19, 40, 0, 0, ZoneOffset.UTC)
    val MORNING: ZonedDateTime = ZonedDateTime.of(2026, 10, 16, 8, 15, 0, 0, ZoneOffset.UTC)
    private val now = AFTERNOON.toInstant().toEpochMilli()
    private const val MINUTE = 60_000L

    private fun huddle(vararg nicks: String) = nicks.map { HuddleParticipant(it, "$it@waddle.test") }

    val rooms = listOf(
        RoomActivity(
            roomJid = "storage@muc.waddle.test",
            name = "storage",
            joined = true,
            here = 6,
            huddle = huddle("mara", "sol", "priya"),
            preview = "Parquet compaction is green on staging",
            previewAuthor = "sol",
            lastUpdated = now - 4 * MINUTE,
        ),
        RoomActivity(
            roomJid = "general@muc.waddle.test",
            name = "general",
            joined = true,
            here = 14,
            mentions = 2,
            unread = 5,
            preview = "@david the Grafana board is pinned in here now",
            previewAuthor = "carol",
            lastUpdated = now - 11 * MINUTE,
        ),
        RoomActivity(
            roomJid = "releases@muc.waddle.test",
            name = "releases",
            joined = true,
            here = 0,
            unread = 12,
            preview = "v0.42 is tagged. Changelog and binaries are up.",
            previewAuthor = "release-bot",
            lastUpdated = now - 38 * MINUTE,
        ),
        RoomActivity(
            roomJid = "design@muc.waddle.test",
            name = "design",
            forum = true,
            joined = true,
            here = 3,
            threadUnread = 2,
            preview = "New tab bar mocks for Android are in the thread",
            previewAuthor = "noor",
            lastUpdated = now - 2 * 60 * MINUTE,
        ),
        RoomActivity(
            roomJid = "random@muc.waddle.test",
            name = "random",
            joined = true,
            here = 0,
            unread = 30,
            muted = true,
            lastUpdated = now - 5 * 60 * MINUTE,
        ),
        RoomActivity(roomJid = "welcome@muc.waddle.test", name = "welcome", joined = true, here = 0),
        RoomActivity(
            roomJid = "events@muc.waddle.test",
            name = "events",
            description = "Meetups, talks and the monthly community call.",
        ),
    )

    private fun person(name: String, show: PresenceShow = PresenceShow.AVAILABLE, huddleRoom: String? = null) =
        PersonRow(
            jid = "$name@waddle.test",
            name = name,
            show = show,
            huddleRoomName = huddleRoom,
            inHuddle = huddleRoom != null,
        )

    val people = listOf(
        person("mara", huddleRoom = "storage"),
        person("sol", huddleRoom = "storage"),
        person("priya", PresenceShow.DND, huddleRoom = "storage"),
        person("carol"),
        person("devon"),
        person("kai"),
        person("noor"),
        person("ines", PresenceShow.DND),
        person("jules"),
        person("tomasz"),
        person("lena", PresenceShow.AWAY),
        person("oskar", PresenceShow.XA),
        person("ravi", PresenceShow.AWAY),
    )

    val busyHome = HomeContentState(
        greetingName = "David",
        rooms = rooms,
        people = people,
        dmUnread = 3,
        connection = ConnectionState.Ready,
        settled = true,
    )

    val quietHome = busyHome.copy(
        rooms = rooms.map { it.copy(huddle = emptyList(), mentions = 0, unread = 0, threadUnread = 0) },
        people = emptyList(),
        dmUnread = 0,
    )

    val loadingHome = busyHome.copy(
        rooms = emptyList(),
        people = emptyList(),
        dmUnread = 0,
        settled = false,
        connection = ConnectionState.Offline,
    )

    val sidebar = SidebarState(
        community = "waddle.test",
        identity = YouIdentity(name = "David Flanagan", jid = "david@waddle.test", connection = ConnectionState.Ready),
        rooms = sidebarRoomsOf(rooms),
        dms = listOf(
            DmListItem(
                DmListRow.Peer("mara@waddle.test", "mara", unreadCount = 2),
                PresenceShow.AVAILABLE,
                inHuddle = true,
            ),
            DmListItem(DmListRow.Peer("lena@waddle.test", "lena", unreadCount = 0), PresenceShow.AWAY),
            DmListItem(DmListRow.Group("infra-dm@muc.waddle.test", "mara, sol, priya", unreadCount = 1)),
        ),
    )
}
