package social.waddle.android.feature.dm

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
import social.waddle.android.client.FakeClientFactory
import social.waddle.android.client.FakeNetworkSignal
import social.waddle.android.client.InMemoryPreferencesDataStore
import social.waddle.android.client.PinnedRandom
import social.waddle.android.client.ReconnectPolicy
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs

@OptIn(ExperimentalCoroutinesApi::class)
class DmListViewModelTest {
    private fun manager(testScope: TestScope): XmppSessionManager = XmppSessionManager(
        sessionPrefs = SessionPrefs(InMemoryPreferencesDataStore()),
        clientFactory = FakeClientFactory(),
        networkSignal = FakeNetworkSignal(),
        userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
        reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
        dispatcher = StandardTestDispatcher(testScope.testScheduler),
    )

    @Before
    fun setUp() {
        Dispatchers.setMain(StandardTestDispatcher())
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    @Test
    fun `peers keep store recency order with localpart names and badges`() = runTest {
        val manager = manager(this)
        manager.dmStore.seed(listOf("alice@waddle.test", "bob@waddle.test"))
        manager.unreadStore.onLiveMessage("bob@waddle.test", isMine = false)
        val viewModel = DmListViewModel(manager)

        viewModel.peers.test {
            skipItems(1)
            runCurrent()
            assertEquals(
                listOf(
                    DmListItem("bob@waddle.test", name = "bob", unreadCount = 1),
                    DmListItem("alice@waddle.test", name = "alice", unreadCount = 0),
                ),
                awaitItem(),
            )
        }
    }
}
