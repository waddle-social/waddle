package social.waddle.android.client

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleFeedEntry
import social.waddle.client.ffi.WaddleFeedSourceKind
import java.time.Instant

/** XEP-0472 community feed verbs and their session cache through the manager. */
@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionManagerFeedTest {
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
    }

    private fun ffiEntry(
        id: String,
        body: String,
        publishedEpochMs: Long? = null,
        source: WaddleFeedSourceKind? = null,
    ) = WaddleFeedEntry(
        id = id,
        title = "Title $id",
        body = body,
        author = "bob@waddle.test",
        publishedEpochMs = publishedEpochMs,
        link = null,
        source = source,
    )

    @Test
    fun `refresh maps the fetched page into the store newest first`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.community.feedEntries = listOf(
            ffiEntry("post-old", "older", publishedEpochMs = 1_000L),
            ffiEntry("post-new", "newer", publishedEpochMs = 2_000L, source = WaddleFeedSourceKind.MOOD),
        )

        assertEquals(VerbResult.Ok, harness.manager.refreshFeed())

        assertEquals(
            listOf(
                FeedEntry(
                    id = "post-new",
                    title = "Title post-new",
                    body = "newer",
                    author = "bob@waddle.test",
                    published = Instant.ofEpochMilli(2_000L),
                    source = FeedSourceKind.MOOD,
                ),
                FeedEntry(
                    id = "post-old",
                    title = "Title post-old",
                    body = "older",
                    author = "bob@waddle.test",
                    published = Instant.ofEpochMilli(1_000L),
                ),
            ),
            harness.manager.feedStore.entries.value,
        )
        assertFalse(harness.manager.feedStore.isLoading.value)
        assertEquals(listOf<UInt?>(50u), harness.client.community.fetchFeedCalls)
        harness.manager.logout()
    }

    @Test
    fun `refresh failure keeps the store and drops the loading flag`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.community.fetchFeedFailure = RuntimeException("boom")

        assertEquals(VerbResult.Rejected, harness.manager.refreshFeed())

        assertEquals(emptyList<FeedEntry>(), harness.manager.feedStore.entries.value)
        assertFalse(harness.manager.feedStore.isLoading.value)
        harness.manager.logout()
    }

    @Test
    fun `a stale fetch resolving after relogin never writes the store`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.community.feedEntries = listOf(ffiEntry("post-stale", "stale", 1_000L))
        harness.client.community.fetchFeedDelayMillis = 1_000

        val stale = launch { harness.manager.refreshFeed() }
        runCurrent()

        // Relogin (same account) while the old session's fetch is parked:
        // the bare-JID-blind generation gate must drop its late write.
        harness.loginReady(this)
        advanceTimeBy(2_000)
        runCurrent()
        stale.join()

        assertEquals(emptyList<FeedEntry>(), harness.manager.feedStore.entries.value)
        assertFalse(harness.manager.feedStore.isLoading.value)
        harness.manager.logout()
    }

    @Test
    fun `publish optimistically prepends then reconciles from the server`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val community = harness.client.community
        community.publishedItemId = "post-42"
        community.feedEntries = listOf(
            ffiEntry("post-42", "hello", publishedEpochMs = 2_000L),
            ffiEntry("post-1", "earlier", publishedEpochMs = 1_000L),
        )
        // Hold the reconciling refresh so the optimistic entry is observable.
        community.fetchFeedDelayMillis = 500

        val publish = launch { harness.manager.publishFeedPost("  hello  ", title = " Hi ") }
        runCurrent()

        val optimistic = harness.manager.feedStore.entries.value
        assertEquals(listOf("post-42"), optimistic.map { it.id })
        assertEquals("hello", optimistic.single().body)
        assertEquals("Hi", optimistic.single().title)
        assertEquals("icepuma@waddle.test", optimistic.single().author)
        assertEquals(listOf("hello" to "Hi"), community.publishFeedCalls)

        advanceTimeBy(1_000)
        runCurrent()
        publish.join()

        assertEquals(
            listOf("post-42", "post-1"),
            harness.manager.feedStore.entries.value.map { it.id },
        )
        harness.manager.logout()
    }

    @Test
    fun `a refused publish leaves the store untouched`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.community.publishFeedFailure = RuntimeException("forbidden")

        assertEquals(VerbResult.Rejected, harness.manager.publishFeedPost("hello"))
        assertEquals(VerbResult.NotReady, harness.manager.publishFeedPost("   "))

        assertEquals(emptyList<FeedEntry>(), harness.manager.feedStore.entries.value)
        assertEquals(0, harness.client.community.fetchFeedCalls.size)
        harness.manager.logout()
    }
}
