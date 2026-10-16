package social.waddle.android.feature.home

import app.cash.turbine.test
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test
import social.waddle.android.client.ConnectionState
import social.waddle.android.client.FakeClientFactory
import social.waddle.android.client.FakeNetworkSignal
import social.waddle.android.client.InMemoryPreferencesDataStore
import social.waddle.android.client.PinnedRandom
import social.waddle.android.client.ReconnectPolicy
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.testSessionInfo
import social.waddle.android.feature.rooms.TOPOLOGY_GRACE_MILLIS
import social.waddle.client.ffi.WaddleChannel
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleSpace
import social.waddle.client.ffi.WaddleTopology

@OptIn(ExperimentalCoroutinesApi::class)
class HomeViewModelTest {
    private class Harness(testScope: TestScope) {
        val factory = FakeClientFactory()
        val manager = XmppSessionManager(
            sessionPrefs = SessionPrefs(InMemoryPreferencesDataStore()),
            clientFactory = factory,
            networkSignal = FakeNetworkSignal(),
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScope.testScheduler),
        )
        val viewModel = HomeViewModel(sessionManager = manager, nick = "icepuma")
    }

    @Before
    fun setUp() {
        Dispatchers.setMain(StandardTestDispatcher())
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    private fun channel(
        id: String,
        roomJid: String,
        name: String,
        position: Int,
        spaceId: String,
        isGroupDm: Boolean = false,
    ) = WaddleChannel(
        id = id,
        roomJid = roomJid,
        name = name,
        description = null,
        channelType = "text",
        position = position,
        spaceId = spaceId,
        autojoin = true,
        bookmarkName = null,
        isGroupDm = isGroupDm,
    )

    private fun space(id: String, name: String) = WaddleSpace(
        id = id,
        serviceJid = "spaces.waddle.test",
        name = name,
        description = null,
    )

    @Test
    fun `rooms carry unread and mentions, and group dms stay off the room list`() = runTest {
        val harness = Harness(this)
        harness.manager.roomStore.setTopology(
            WaddleTopology(
                spaces = listOf(space("s1", "Waddle HQ")),
                channels = listOf(
                    channel("c2", "random@muc.waddle.test", "random", position = 2, spaceId = "s1"),
                    channel("c1", "general@muc.waddle.test", "general", position = 1, spaceId = "s1"),
                    channel("g1", "gdm@muc.waddle.test", "trio", position = 3, spaceId = "s1", isGroupDm = true),
                ),
            ),
        )
        harness.manager.unreadStore.onLiveMessage("general@muc.waddle.test", isMine = false, mentionsSelf = true)
        harness.manager.unreadStore.onLiveMessage("general@muc.waddle.test", isMine = false)

        harness.viewModel.uiState.test {
            skipItems(1)
            runCurrent()
            val rooms = expectMostRecentItem().rooms
            assertEquals(listOf("random", "general"), rooms.map { it.name })
            val general = rooms.single { it.name == "general" }
            assertEquals(2, general.unread)
            assertEquals(1, general.mentions)
            assertEquals(false, general.joined)
            cancelAndIgnoreRemainingEvents()
        }
    }

    @Test
    fun `the topology counts as settled once it has channels`() = runTest {
        val harness = Harness(this)

        harness.viewModel.uiState.test {
            assertEquals(false, awaitItem().settled)
            harness.manager.roomStore.setTopology(
                WaddleTopology(
                    spaces = emptyList(),
                    channels = listOf(channel("c1", "general@muc.waddle.test", "general", 1, "s1")),
                ),
            )
            runCurrent()
            assertEquals(true, expectMostRecentItem().settled)
            cancelAndIgnoreRemainingEvents()
        }
    }

    @Test
    fun `an empty community settles after the ready grace`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        harness.viewModel.uiState.test {
            runCurrent()
            assertEquals(false, expectMostRecentItem().settled)
            advanceTimeBy(TOPOLOGY_GRACE_MILLIS + 1)
            runCurrent()
            assertEquals(true, expectMostRecentItem().settled)
            cancelAndIgnoreRemainingEvents()
        }
        harness.manager.logout()
    }

    @Test
    fun `dm unread count aggregates over dm peers only`() = runTest {
        val harness = Harness(this)
        harness.manager.dmStore.seed(listOf("alice@waddle.test", "bob@waddle.test"))
        harness.manager.unreadStore.onLiveMessage("alice@waddle.test", isMine = false)
        harness.manager.unreadStore.onLiveMessage("alice@waddle.test", isMine = false)
        harness.manager.unreadStore.onLiveMessage("bob@waddle.test", isMine = false)
        // Channel unread must not leak into the DM badge.
        harness.manager.unreadStore.onLiveMessage("general@muc.waddle.test", isMine = false)

        harness.viewModel.uiState.test {
            skipItems(1)
            runCurrent()
            assertEquals(3, expectMostRecentItem().dmUnreadCount)
        }
    }

    @Test
    fun `dm unread count includes group dm rooms from the dm surface`() = runTest {
        val harness = Harness(this)
        harness.manager.dmStore.seed(listOf("alice@waddle.test"))
        harness.manager.roomStore.setTopology(
            WaddleTopology(
                spaces = emptyList(),
                channels = listOf(
                    social.waddle.android.client.testChannel(
                        "gdm-1@muc.waddle.test",
                        name = "Alice, Bob",
                        isGroupDm = true,
                    ),
                    social.waddle.android.client.testChannel("general@muc.waddle.test"),
                ),
            ),
        )
        harness.manager.unreadStore.onLiveMessage("alice@waddle.test", isMine = false)
        harness.manager.unreadStore.onLiveMessage("gdm-1@muc.waddle.test", isMine = false)
        // Regular channel unread still stays off the DM badge.
        harness.manager.unreadStore.onLiveMessage("general@muc.waddle.test", isMine = false)

        harness.viewModel.uiState.test {
            skipItems(1)
            runCurrent()
            assertEquals(2, expectMostRecentItem().dmUnreadCount)
        }
    }

    @Test
    fun `connection state passes through to the ui state`() = runTest {
        val harness = Harness(this)

        harness.viewModel.uiState.test {
            assertEquals(ConnectionState.Idle, awaitItem().connectionState)

            harness.manager.login(testSessionInfo())
            runCurrent()
            assertEquals(ConnectionState.Connecting, awaitItem().connectionState)

            harness.factory.emit(WaddleClientEvent.Connected)
            runCurrent()
            assertEquals(ConnectionState.Ready, awaitItem().connectionState)
        }

        harness.manager.logout()
    }

    @Test
    fun `open channel joins the room with the account nick`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        harness.viewModel.openChannel("general@muc.waddle.test")
        runCurrent()

        assertEquals(
            listOf("general@muc.waddle.test" to "icepuma"),
            harness.factory.clients.single().joinRoomCalls,
        )

        harness.manager.logout()
    }
}
