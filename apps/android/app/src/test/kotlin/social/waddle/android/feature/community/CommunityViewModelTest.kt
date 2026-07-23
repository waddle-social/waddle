package social.waddle.android.feature.community

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
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
import social.waddle.android.client.FakeClientFactory
import social.waddle.android.client.FakeNetworkSignal
import social.waddle.android.client.FeedSourceKind
import social.waddle.android.client.InMemoryPreferencesDataStore
import social.waddle.android.client.PinnedRandom
import social.waddle.android.client.ReconnectPolicy
import social.waddle.android.client.VerbResult
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.testFeedEntry
import social.waddle.android.client.testSessionInfo
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleFeedSourceKind

@OptIn(ExperimentalCoroutinesApi::class)
class CommunityViewModelTest {
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

        suspend fun loginReady(scope: TestScope) {
            manager.login(testSessionInfo())
            scope.runCurrent()
            factory.emit(WaddleClientEvent.Connected)
            scope.runCurrent()
        }

        val client get() = factory.clients.last()

        fun viewModel() = CommunityViewModel(sessionManager = manager)
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
    fun `init refreshes the feed into the store newest first`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.community.feedEntries = listOf(
            testFeedEntry(id = "post-old", body = "older", publishedEpochMs = 1_000L),
            testFeedEntry(
                id = "post-new",
                body = "newer",
                publishedEpochMs = 2_000L,
                source = WaddleFeedSourceKind.TUNE,
            ),
        )

        val viewModel = harness.viewModel()
        runCurrent()

        assertEquals(listOf("post-new", "post-old"), viewModel.entries.value.map { it.id })
        assertEquals(FeedSourceKind.TUNE, viewModel.entries.value.first().source)
        assertFalse(viewModel.isLoading.value)
        harness.manager.logout()
    }

    @Test
    fun `post publishes through the manager and reports the result`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.community.publishedItemId = "post-9"
        val viewModel = harness.viewModel()
        runCurrent()
        // The reconciling refresh after publish serves the server's
        // view, which includes the accepted post.
        harness.client.community.feedEntries = listOf(
            testFeedEntry(id = "post-9", body = "hello feed", publishedEpochMs = 3_000L),
        )

        val results = mutableListOf<VerbResult>()
        backgroundScope.launch(StandardTestDispatcher(testScheduler)) {
            viewModel.postResults.collect { results += it }
        }
        runCurrent()

        viewModel.post("hello feed")
        runCurrent()

        assertEquals(listOf<VerbResult>(VerbResult.Ok), results)
        assertEquals(
            listOf("hello feed" to null),
            harness.client.community.publishFeedCalls.toList(),
        )
        assertEquals("post-9", viewModel.entries.value.first().id)
        assertFalse(viewModel.isPosting.value)
        harness.manager.logout()
    }

    @Test
    fun `a failed load raises the error banner instead of the empty state`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.community.fetchFeedFailure = RuntimeException("stream broke")

        val viewModel = harness.viewModel()
        runCurrent()

        assertTrue(viewModel.loadFailed.value)
        assertTrue(viewModel.entries.value.isEmpty())

        // A successful pull-to-refresh clears the banner.
        harness.client.community.fetchFeedFailure = null
        harness.client.community.feedEntries = listOf(
            testFeedEntry(id = "post-1", body = "back online", publishedEpochMs = 1_000L),
        )
        viewModel.refresh()
        runCurrent()

        assertFalse(viewModel.loadFailed.value)
        assertEquals(listOf("post-1"), viewModel.entries.value.map { it.id })
        harness.manager.logout()
    }

    @Test
    fun `a successful post clears the load-failure banner`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.community.fetchFeedFailure = RuntimeException("stream broke")
        val viewModel = harness.viewModel()
        runCurrent()
        assertTrue(viewModel.loadFailed.value)

        // Server recovers; the publish succeeds and its reconciling
        // refresh loads the feed — the stale banner must clear.
        harness.client.community.fetchFeedFailure = null
        harness.client.community.publishedItemId = "post-2"
        harness.client.community.feedEntries = listOf(
            testFeedEntry(id = "post-2", body = "hello again", publishedEpochMs = 2_000L),
        )
        viewModel.post("hello again")
        runCurrent()

        assertFalse(viewModel.loadFailed.value)
        assertEquals(listOf("post-2"), viewModel.entries.value.map { it.id })
        harness.manager.logout()
    }

    @Test
    fun `a refused post surfaces the failure result`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.community.publishFeedFailure = RuntimeException("forbidden")
        val viewModel = harness.viewModel()
        runCurrent()

        val results = mutableListOf<VerbResult>()
        backgroundScope.launch(StandardTestDispatcher(testScheduler)) {
            viewModel.postResults.collect { results += it }
        }
        runCurrent()

        viewModel.post("hello feed")
        runCurrent()

        assertEquals(listOf<VerbResult>(VerbResult.Rejected), results)
        assertEquals(0, viewModel.entries.value.size)
        harness.manager.logout()
    }
}
