package social.waddle.android.feature.search

import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.cancel
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import social.waddle.android.client.FakeClientFactory
import social.waddle.android.client.FakeNetworkSignal
import social.waddle.android.client.InMemoryPreferencesDataStore
import social.waddle.android.client.PinnedRandom
import social.waddle.android.client.ReconnectPolicy
import social.waddle.android.client.SearchCall
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.testArchivedMessage
import social.waddle.android.client.testMamPage
import social.waddle.android.client.testSessionInfo
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleMamPage

/**
 * The search query state machine end-to-end through the real session
 * manager into the fake FFI client: debounce, the stale-response race
 * guard, the empty/failed states, and room-vs-DM verb routing.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class MessageSearchViewModelTest {
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
            factory.emitReady()
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

    private val roomTarget = MessageSearchTarget(ROOM_JID, isGroupchat = true)
    private val dmTarget = MessageSearchTarget(PEER_JID, isGroupchat = false)

    private fun matchPage(mamId: String, body: String): WaddleMamPage =
        testMamPage(messages = listOf(testArchivedMessage(mamId = mamId, body = body)))

    @Test
    fun `debounce collapses rapid typing into one trimmed search`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = MessageSearchViewModel(harness.manager, roomTarget)
        runCurrent()

        viewModel.onQueryChanged("p")
        advanceTimeBy(100)
        viewModel.onQueryChanged("pe")
        advanceTimeBy(100)
        viewModel.onQueryChanged(" penguin ")
        // One tick short of the window: nothing may fire yet.
        advanceTimeBy(MessageSearchViewModel.DEBOUNCE_MILLIS - 1)
        runCurrent()
        assertTrue(harness.client.searchCalls.isEmpty())

        advanceTimeBy(1)
        runCurrent()
        assertEquals(
            listOf(SearchCall(ROOM_JID, "penguin", MessageSearchViewModel.MAX_RESULTS, isRoom = true)),
            harness.client.searchCalls,
        )
        harness.manager.logout()
    }

    @Test
    fun `stale response never clobbers a newer query's results`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = MessageSearchViewModel(harness.manager, roomTarget)
        runCurrent()

        // First query parks on an uncompleted deferred.
        val stale = CompletableDeferred<WaddleMamPage>()
        harness.client.searchResponses += stale
        viewModel.onQueryChanged("first")
        advanceTimeBy(MessageSearchViewModel.DEBOUNCE_MILLIS)
        runCurrent()
        assertEquals(MessageSearchState.Searching, viewModel.state.value)

        // Second query answers immediately with its own page.
        harness.client.mamPage = matchPage(mamId = "m2", body = "second match")
        viewModel.onQueryChanged("second")
        advanceTimeBy(MessageSearchViewModel.DEBOUNCE_MILLIS)
        runCurrent()
        val results = viewModel.state.value as MessageSearchState.Results
        assertEquals("m2", results.hits.single().key)

        // The first response lands late: the race guard must drop it.
        stale.complete(matchPage(mamId = "m1", body = "stale match"))
        runCurrent()
        assertEquals("m2", (viewModel.state.value as MessageSearchState.Results).hits.single().key)
        assertEquals(2, harness.client.searchCalls.size)
        harness.manager.logout()
    }

    @Test
    fun `page without displayable matches surfaces the empty state`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = MessageSearchViewModel(harness.manager, roomTarget)
        runCurrent()
        // Retracted and bodyless matches are filtered, so this page is empty.
        harness.client.mamPage = testMamPage(
            messages = listOf(
                testArchivedMessage(mamId = "m1", body = "gone", isRetracted = true),
                testArchivedMessage(mamId = "m2", body = null),
            ),
        )

        viewModel.onQueryChanged("penguin")
        advanceTimeBy(MessageSearchViewModel.DEBOUNCE_MILLIS)
        runCurrent()

        assertEquals(MessageSearchState.Empty, viewModel.state.value)
        harness.manager.logout()
    }

    @Test
    fun `an archive error page surfaces failed, not empty`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = MessageSearchViewModel(harness.manager, roomTarget)
        runCurrent()
        // The FFI reports query errors as an empty INCOMPLETE page —
        // distinct from a genuine zero-hit complete fin.
        harness.client.mamPage = testMamPage(messages = emptyList(), isComplete = false)

        viewModel.onQueryChanged("penguin")
        advanceTimeBy(MessageSearchViewModel.DEBOUNCE_MILLIS)
        runCurrent()

        assertEquals(MessageSearchState.Failed, viewModel.state.value)
        harness.manager.logout()
    }

    @Test
    fun `search without a live session surfaces the failed state`() = runTest {
        val harness = Harness(this)
        // Login without ever reaching ready: the verbs answer null.
        harness.manager.login(testSessionInfo())
        runCurrent()
        val viewModel = MessageSearchViewModel(harness.manager, roomTarget)
        runCurrent()

        viewModel.onQueryChanged("penguin")
        advanceTimeBy(MessageSearchViewModel.DEBOUNCE_MILLIS)
        runCurrent()

        assertEquals(MessageSearchState.Failed, viewModel.state.value)
        harness.manager.logout()
    }

    @Test
    fun `clearing the query resets to idle and drops the in-flight response`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = MessageSearchViewModel(harness.manager, roomTarget)
        runCurrent()

        val parked = CompletableDeferred<WaddleMamPage>()
        harness.client.searchResponses += parked
        viewModel.onQueryChanged("penguin")
        advanceTimeBy(MessageSearchViewModel.DEBOUNCE_MILLIS)
        runCurrent()
        assertEquals(MessageSearchState.Searching, viewModel.state.value)

        viewModel.onQueryChanged("")
        runCurrent()
        assertEquals(MessageSearchState.Idle, viewModel.state.value)

        // The abandoned response must not resurrect results.
        parked.complete(matchPage(mamId = "m1", body = "late match"))
        runCurrent()
        assertEquals(MessageSearchState.Idle, viewModel.state.value)
        harness.manager.logout()
    }

    @Test
    fun `sheet clear resets to idle, drops in-flight responses, and re-arms the same query`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = MessageSearchViewModel(harness.manager, roomTarget)
        runCurrent()

        val parked = CompletableDeferred<WaddleMamPage>()
        harness.client.searchResponses += parked
        viewModel.onQueryChanged("penguin")
        advanceTimeBy(MessageSearchViewModel.DEBOUNCE_MILLIS)
        runCurrent()
        assertEquals(MessageSearchState.Searching, viewModel.state.value)

        // The sheet-dismiss path: clear() rather than an emptied field.
        viewModel.clear()
        runCurrent()
        assertEquals("", viewModel.query.value)
        assertEquals(MessageSearchState.Idle, viewModel.state.value)

        // The abandoned response must not resurrect results after close.
        parked.complete(matchPage(mamId = "m1", body = "late match"))
        runCurrent()
        assertEquals(MessageSearchState.Idle, viewModel.state.value)

        // Reopen: the identical query searches again from scratch.
        harness.client.mamPage = matchPage(mamId = "m2", body = "fresh match")
        viewModel.onQueryChanged("penguin")
        advanceTimeBy(MessageSearchViewModel.DEBOUNCE_MILLIS)
        runCurrent()
        assertEquals(
            listOf("fresh match"),
            (viewModel.state.value as MessageSearchState.Results).hits.map { it.body },
        )
        harness.manager.logout()
    }

    @Test
    fun `cancellation mid-search propagates instead of reporting failure`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = MessageSearchViewModel(harness.manager, roomTarget)
        runCurrent()

        harness.client.searchResponses += CompletableDeferred()
        viewModel.onQueryChanged("penguin")
        advanceTimeBy(MessageSearchViewModel.DEBOUNCE_MILLIS)
        runCurrent()
        assertEquals(MessageSearchState.Searching, viewModel.state.value)

        // Screen teardown: the in-flight search is cancelled, and the
        // cancellation must propagate — not be swallowed into Failed.
        viewModel.viewModelScope.cancel()
        runCurrent()

        assertEquals(MessageSearchState.Searching, viewModel.state.value)
        harness.manager.logout()
    }

    @Test
    fun `room and dm targets route to their own verbs`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.mamPage = matchPage(mamId = "m1", body = "penguin fact")
        val roomViewModel = MessageSearchViewModel(harness.manager, roomTarget)
        val dmViewModel = MessageSearchViewModel(harness.manager, dmTarget)
        runCurrent()

        roomViewModel.onQueryChanged("penguin")
        advanceTimeBy(MessageSearchViewModel.DEBOUNCE_MILLIS)
        runCurrent()
        dmViewModel.onQueryChanged("penguin")
        advanceTimeBy(MessageSearchViewModel.DEBOUNCE_MILLIS)
        runCurrent()

        assertEquals(
            listOf(
                SearchCall(ROOM_JID, "penguin", MessageSearchViewModel.MAX_RESULTS, isRoom = true),
                SearchCall(PEER_JID, "penguin", MessageSearchViewModel.MAX_RESULTS, isRoom = false),
            ),
            harness.client.searchCalls,
        )
        assertEquals(
            listOf("penguin fact"),
            (roomViewModel.state.value as MessageSearchState.Results).hits.map { it.body },
        )
        assertEquals(
            listOf("penguin fact"),
            (dmViewModel.state.value as MessageSearchState.Results).hits.map { it.body },
        )
        harness.manager.logout()
    }

    private companion object {
        const val ROOM_JID = "room@muc.waddle.test"
        const val PEER_JID = "alice@waddle.test"
    }
}
