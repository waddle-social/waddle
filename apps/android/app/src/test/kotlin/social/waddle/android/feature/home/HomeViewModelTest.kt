package social.waddle.android.feature.home

import app.cash.turbine.test
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
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
    ) = WaddleChannel(
        id = id,
        roomJid = roomJid,
        name = name,
        description = null,
        channelType = "text",
        position = position,
        spaceId = spaceId,
    )

    private fun space(id: String, name: String) = WaddleSpace(
        id = id,
        serviceJid = "spaces.waddle.test",
        name = name,
        description = null,
    )

    @Test
    fun `sections sort channels by position and map unread counts`() = runTest {
        val harness = Harness(this)
        harness.manager.roomStore.setTopology(
            WaddleTopology(
                spaces = listOf(space("s1", "Waddle HQ")),
                channels = listOf(
                    channel("c2", "random@muc.waddle.test", "random", position = 2, spaceId = "s1"),
                    channel("c1", "general@muc.waddle.test", "general", position = 1, spaceId = "s1"),
                ),
            ),
        )
        harness.manager.unreadStore.onLiveMessage("general@muc.waddle.test", isMine = false)
        harness.manager.unreadStore.onLiveMessage("general@muc.waddle.test", isMine = false)

        harness.viewModel.uiState.test {
            skipItems(1)
            runCurrent()
            assertEquals(
                listOf(
                    SpaceSection(
                        id = "s1",
                        name = "Waddle HQ",
                        channels = listOf(
                            ChannelListItem("general@muc.waddle.test", "general", unreadCount = 2),
                            ChannelListItem("random@muc.waddle.test", "random", unreadCount = 0),
                        ),
                    ),
                ),
                awaitItem().sections,
            )
        }
    }

    @Test
    fun `channels of unknown spaces collect into the unnamed orphan bucket`() = runTest {
        val harness = Harness(this)
        harness.manager.roomStore.setTopology(
            WaddleTopology(
                spaces = listOf(space("s1", "Waddle HQ")),
                channels = listOf(
                    channel("c1", "general@muc.waddle.test", "general", position = 1, spaceId = "s1"),
                    channel("c9", "stray-b@muc.waddle.test", "stray-b", position = 9, spaceId = "gone"),
                    channel("c8", "stray-a@muc.waddle.test", "stray-a", position = 8, spaceId = "gone"),
                ),
            ),
        )

        harness.viewModel.uiState.test {
            skipItems(1)
            runCurrent()
            val sections = awaitItem().sections
            assertEquals(2, sections.size)
            val orphans = sections.last()
            assertEquals(null, orphans.name)
            assertEquals(
                listOf("stray-a@muc.waddle.test", "stray-b@muc.waddle.test"),
                orphans.channels.map { it.roomJid },
            )
        }
    }

    @Test
    fun `no orphan section appears when every channel has its space`() = runTest {
        val harness = Harness(this)
        harness.manager.roomStore.setTopology(
            WaddleTopology(
                spaces = listOf(space("s1", "Waddle HQ")),
                channels = listOf(
                    channel("c1", "general@muc.waddle.test", "general", position = 1, spaceId = "s1"),
                ),
            ),
        )

        harness.viewModel.uiState.test {
            skipItems(1)
            runCurrent()
            assertEquals(listOf("s1"), awaitItem().sections.map { it.id })
        }
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
            assertEquals(3, awaitItem().dmUnreadCount)
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
