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
import social.waddle.android.client.store.NotifySettingsEntry
import social.waddle.android.client.testChannel
import social.waddle.android.client.testInboxEntry
import social.waddle.client.ffi.WaddleNotifyMode
import social.waddle.client.ffi.WaddleTopology

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

        viewModel.rows.test {
            skipItems(1)
            runCurrent()
            assertEquals(
                listOf(
                    DmListRow.Peer("bob@waddle.test", name = "bob", unreadCount = 1),
                    DmListRow.Peer("alice@waddle.test", name = "alice", unreadCount = 0),
                ),
                awaitItem(),
            )
        }
    }

    @Test
    fun `group dms merge into the surface by inbox recency, channels never`() = runTest {
        val manager = manager(this)
        manager.dmStore.seed(listOf("alice@waddle.test", "bob@waddle.test"))
        manager.roomStore.setTopology(
            WaddleTopology(
                spaces = emptyList(),
                channels = listOf(
                    testChannel("general@muc.waddle.test"),
                    testChannel("gdm-1@muc.waddle.test", name = "Alice, Bob", isGroupDm = true),
                ),
            ),
        )
        // Inbox recency: the group DM is newer than alice; bob has no
        // inbox entry and keeps the DmStore seed position at the tail.
        manager.inboxStore.applyEntry(
            testInboxEntry(partner = "gdm-1@muc.waddle.test", kind = "muc", lastUpdated = 200L, unread = 0u),
        )
        manager.inboxStore.applyEntry(
            testInboxEntry(partner = "alice@waddle.test", kind = "direct", lastUpdated = 100L, unread = 0u),
        )
        manager.unreadStore.onLiveMessage("gdm-1@muc.waddle.test", isMine = false)

        val viewModel = DmListViewModel(manager)
        viewModel.rows.test {
            skipItems(1)
            runCurrent()
            assertEquals(
                listOf(
                    DmListRow.Group("gdm-1@muc.waddle.test", name = "Alice, Bob", unreadCount = 1),
                    DmListRow.Peer("alice@waddle.test", name = "alice", unreadCount = 0),
                    DmListRow.Peer("bob@waddle.test", name = "bob", unreadCount = 0),
                ),
                awaitItem(),
            )
        }
    }

    @Test
    fun `merge orders timed rows first and stamps mute overrides`() {
        val rows = mergedDmRows(
            DmSurfaceInputs(
                peers = listOf("alice@waddle.test", "carol@waddle.test"),
                groupDms = listOf(
                    testChannel("gdm-old@muc.waddle.test", name = "Old crew", isGroupDm = true),
                    testChannel("gdm-quiet@muc.waddle.test", name = "Quiet", isGroupDm = true),
                ),
                directInbox = mapOf(
                    "alice@waddle.test" to
                        testSnapshot("alice@waddle.test", lastUpdated = 300L),
                ),
                mucInbox = mapOf(
                    "gdm-old@muc.waddle.test" to
                        testSnapshot("gdm-old@muc.waddle.test", lastUpdated = 100L),
                ),
                counts = mapOf("gdm-old@muc.waddle.test" to 4),
                notifyEntries = mapOf(
                    "gdm-old@muc.waddle.test" to
                        NotifySettingsEntry(notifyMode = WaddleNotifyMode.NEVER, richPayloadOptIn = false),
                ),
            ),
        )
        assertEquals(
            listOf(
                DmListRow.Peer("alice@waddle.test", name = "alice", unreadCount = 0),
                DmListRow.Group(
                    "gdm-old@muc.waddle.test",
                    name = "Old crew",
                    unreadCount = 4,
                    isMuted = true,
                ),
                // Untimed rows: peers in store order, then groups.
                DmListRow.Peer("carol@waddle.test", name = "carol", unreadCount = 0),
                DmListRow.Group("gdm-quiet@muc.waddle.test", name = "Quiet", unreadCount = 0),
            ),
            rows,
        )
    }

    private fun testSnapshot(partner: String, lastUpdated: Long) =
        social.waddle.android.client.store.InboxEntrySnapshot(
            partner = partner,
            kind = social.waddle.android.client.store.InboxKind.DIRECT,
            lastStanzaId = "s1",
            lastUpdated = lastUpdated,
            unread = 0,
            preview = null,
            threadId = null,
            threadTitle = null,
            replyCount = null,
            author = null,
        )
}
