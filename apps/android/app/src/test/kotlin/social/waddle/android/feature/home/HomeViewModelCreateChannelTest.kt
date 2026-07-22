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
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import social.waddle.android.client.CreateRoomResult
import social.waddle.android.client.FakeClientFactory
import social.waddle.android.client.FakeNetworkSignal
import social.waddle.android.client.InMemoryPreferencesDataStore
import social.waddle.android.client.PinnedRandom
import social.waddle.android.client.ReconnectPolicy
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.testSessionInfo
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleException

/** Create-channel gating + submit through the home view model. */
@OptIn(ExperimentalCoroutinesApi::class)
class HomeViewModelCreateChannelTest {
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

        suspend fun loginReady(scope: TestScope) {
            manager.login(testSessionInfo())
            scope.runCurrent()
            factory.emit(WaddleClientEvent.Connected)
            scope.runCurrent()
        }

        val client get() = factory.clients.last()
    }

    @Before
    fun setUp() {
        Dispatchers.setMain(StandardTestDispatcher())
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    @Test
    fun `create affordance is gated on the community-owner probe`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.client.communityOwner = true
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        assertTrue(harness.viewModel.canCreateChannel.value)
    }

    @Test
    fun `non-owners never see the create affordance`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        runCurrent()
        assertFalse(harness.viewModel.canCreateChannel.value)
    }

    @Test
    fun `createChannel slugs the name and reports the room jid`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)

        harness.viewModel.createEvents.test {
            harness.viewModel.createChannel("Design Reviews", " weekly crits ", forum = true)
            runCurrent()

            val created = awaitItem() as CreateRoomResult.Created
            assertEquals("design-reviews@muc.waddle.test", created.roomJid)
        }
        val (localpart, nick, patch) = harness.client.createRoomCalls.single()
        assertEquals("design-reviews", localpart)
        assertEquals("icepuma", nick)
        assertEquals("Design Reviews", patch.name)
        assertEquals("weekly crits", patch.description)
        assertEquals(true, patch.forum)
    }

    @Test
    fun `a forbidden create surfaces as NotPermitted`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.createRoomFailure = WaddleException.Stanza("forbidden", null)

        harness.viewModel.createEvents.test {
            harness.viewModel.createChannel("general", "", forum = false)
            runCurrent()
            assertEquals(CreateRoomResult.NotPermitted, awaitItem())
        }
    }
}
