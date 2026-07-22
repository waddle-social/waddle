package social.waddle.android.feature.dm

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
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
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
import social.waddle.android.client.testSessionInfo
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleUserSearchEntry

@OptIn(ExperimentalCoroutinesApi::class)
class NewGroupDmViewModelTest {
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

    @Before
    fun setUp() {
        Dispatchers.setMain(StandardTestDispatcher())
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    private fun entry(jid: String, username: String = jid.substringBefore('@')) =
        WaddleUserSearchEntry(jid = jid, username = username, displayName = null)

    @Test
    fun `create is gated on two selected members and never fires below`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = NewGroupDmViewModel(harness.manager, selfJid = "icepuma@waddle.test")

        assertFalse(viewModel.uiState.value.canCreate)
        viewModel.toggleMember(entry("alice@waddle.test"))
        assertFalse(viewModel.uiState.value.canCreate)

        viewModel.create { _, _ -> }
        runCurrent()
        assertTrue(harness.client.groupDm.createCalls.isEmpty())

        viewModel.toggleMember(entry("bob@waddle.test"))
        assertTrue(viewModel.uiState.value.canCreate)
    }

    @Test
    fun `create includes self and falls back to the comma-joined default name`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.groupDm.createdRoomJid = "gdm-9@muc.waddle.test"
        val viewModel = NewGroupDmViewModel(harness.manager, selfJid = "icepuma@waddle.test")
        viewModel.toggleMember(entry("alice@waddle.test"))
        viewModel.toggleMember(entry("bob@waddle.test"))
        assertEquals("alice, bob", viewModel.uiState.value.defaultName)

        var created: Pair<String, String>? = null
        viewModel.create { roomJid, name -> created = roomJid to name }
        runCurrent()

        assertEquals("gdm-9@muc.waddle.test" to "alice, bob", created)
        assertEquals(
            "alice, bob" to listOf("icepuma@waddle.test", "alice@waddle.test", "bob@waddle.test"),
            harness.client.groupDm.createCalls.single(),
        )
    }

    @Test
    fun `a typed name beats the default and a failure surfaces without navigation`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.groupDm.createFailure = RuntimeException("nope")
        val viewModel = NewGroupDmViewModel(harness.manager, selfJid = "icepuma@waddle.test")
        viewModel.toggleMember(entry("alice@waddle.test"))
        viewModel.toggleMember(entry("bob@waddle.test"))
        viewModel.onNameChanged("  Weekend crew  ")

        var created: Pair<String, String>? = null
        viewModel.create { roomJid, name -> created = roomJid to name }
        runCurrent()

        assertNull(created)
        assertTrue(viewModel.uiState.value.createFailed)
        assertFalse(viewModel.uiState.value.isSubmitting)
        assertEquals("Weekend crew", harness.client.groupDm.createCalls.single().first)
    }

    @Test
    fun `a prefilled peer counts toward the minimum (start group from a dm)`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = NewGroupDmViewModel(
            harness.manager,
            selfJid = "icepuma@waddle.test",
            initialMembers = mapOf("alice@waddle.test" to "alice"),
        )

        assertFalse(viewModel.uiState.value.canCreate)
        viewModel.toggleMember(entry("bob@waddle.test"))
        assertTrue(viewModel.uiState.value.canCreate)
    }

    @Test
    fun `search debounces and filters out self and already-selected members`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.directory.userSearchResults = listOf(
            entry("icepuma@waddle.test"),
            entry("alice@waddle.test"),
            entry("carol@waddle.test"),
        )
        val viewModel = NewGroupDmViewModel(
            harness.manager,
            selfJid = "icepuma@waddle.test",
            initialMembers = mapOf("alice@waddle.test" to "alice"),
        )

        viewModel.onSearchQueryChanged("waddle")
        runCurrent()
        assertTrue(viewModel.uiState.value.searchResults.isEmpty())

        advanceTimeBy(NewGroupDmViewModel.SEARCH_DEBOUNCE_MS + 1)
        runCurrent()
        assertEquals(
            listOf("carol@waddle.test"),
            viewModel.uiState.value.searchResults.map { it.jid },
        )
        assertEquals(listOf("waddle"), harness.client.directory.searchUsersCalls.toList())
    }
}
