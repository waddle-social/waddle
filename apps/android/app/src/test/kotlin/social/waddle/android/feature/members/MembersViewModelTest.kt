package social.waddle.android.feature.members

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
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import social.waddle.android.client.FakeClientFactory
import social.waddle.android.client.FakeNetworkSignal
import social.waddle.android.client.HAT_URI_BOT
import social.waddle.android.client.InMemoryPreferencesDataStore
import social.waddle.android.client.PinnedRandom
import social.waddle.android.client.ReconnectPolicy
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.store.MemberListStatus
import social.waddle.android.client.testPresence
import social.waddle.android.client.testSessionInfo
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleMucAffiliation
import social.waddle.client.ffi.WaddlePresenceHat
import social.waddle.client.ffi.WaddleRoomBot
import social.waddle.client.ffi.WaddleRoomMemberEntry
import social.waddle.client.ffi.WaddleUserSearchEntry

@OptIn(ExperimentalCoroutinesApi::class)
class MembersViewModelTest {
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

        fun viewModel() = MembersViewModel(
            sessionManager = manager,
            roomJid = ROOM,
        )
    }

    @Before
    fun setUp() {
        Dispatchers.setMain(StandardTestDispatcher())
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    private fun entry(
        jid: String,
        affiliation: WaddleMucAffiliation,
        nick: String? = null,
    ) = WaddleRoomMemberEntry(jid = jid, affiliation = affiliation, nick = nick, reason = null)

    private fun seedSelfAsOwner(harness: Harness) {
        harness.factory.emit(
            WaddleClientEvent.Presence(
                testPresence(
                    from = "$ROOM/icepuma",
                    mucAffiliation = WaddleMucAffiliation.OWNER,
                    mucJid = "icepuma@waddle.test/app",
                    mucStatusCodes = listOf(110u),
                ),
            ),
        )
    }

    @Test
    fun `rows sort by tier and merge live occupancy with hats`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.roomMembersByTier = mapOf(
            WaddleMucAffiliation.MEMBER to listOf(entry("bob@waddle.test", WaddleMucAffiliation.MEMBER)),
            WaddleMucAffiliation.OWNER to listOf(entry("icepuma@waddle.test", WaddleMucAffiliation.OWNER)),
        )
        seedSelfAsOwner(harness)
        harness.factory.emit(
            WaddleClientEvent.Presence(
                testPresence(
                    from = "$ROOM/bob",
                    mucAffiliation = WaddleMucAffiliation.MEMBER,
                    mucJid = "bob@waddle.test/phone",
                    hats = listOf(WaddlePresenceHat(uri = "urn:waddle:hats:mod", title = "Helper")),
                ),
            ),
        )
        runCurrent()

        val viewModel = harness.viewModel()
        runCurrent()

        val state = viewModel.uiState.value
        assertEquals(MemberListStatus.LOADED, state.status)
        assertTrue(state.canManage)
        // Owner tier first (web sort parity); bob is live with nick + hat.
        assertEquals(listOf("icepuma@waddle.test", "bob@waddle.test"), state.rows.map { it.jid })
        val bob = state.rows.last()
        assertTrue(bob.presentNow)
        assertEquals("bob", bob.nick)
        assertEquals(listOf("Helper"), bob.hats)
        assertFalse(bob.inferred)
    }

    @Test
    fun `occupants missing from the list surface as inferred read-only rows`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        seedSelfAsOwner(harness)
        harness.factory.emit(
            WaddleClientEvent.Presence(
                testPresence(
                    from = "$ROOM/drifter",
                    mucAffiliation = WaddleMucAffiliation.NONE,
                    mucJid = "drifter@waddle.test/web",
                ),
            ),
        )
        runCurrent()

        val viewModel = harness.viewModel()
        runCurrent()

        val drifter = viewModel.uiState.value.rows.single { it.displayName == "drifter" }
        assertTrue(drifter.inferred)
        assertTrue(drifter.presentNow)
    }

    private fun bot(jid: String, name: String?) = WaddleRoomBot(jid = jid, name = name)

    /** A bot's transient occupancy: joins hatted with no affiliation. */
    private fun botPresence(
        nick: String,
        jid: String,
        type: String = "available",
        room: String = ROOM,
    ) = WaddleClientEvent.Presence(
        testPresence(
            from = "$room/$nick",
            presenceType = type,
            mucAffiliation = WaddleMucAffiliation.NONE,
            mucJid = "$jid/bot",
            hats = listOf(WaddlePresenceHat(uri = HAT_URI_BOT, title = "Bot")),
        ),
    )

    @Test
    fun `declared room bots list by name apart from the people`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.roomMembersByTier = mapOf(
            WaddleMucAffiliation.MEMBER to listOf(entry("bob@waddle.test", WaddleMucAffiliation.MEMBER)),
            WaddleMucAffiliation.OWNER to listOf(entry("icepuma@waddle.test", WaddleMucAffiliation.OWNER)),
        )
        harness.client.roomBots = mapOf(
            ROOM to listOf(
                bot("zeta@extensions.waddle.test", "Zeta Bot"),
                // No name from the service: the JID localpart stands in.
                bot("alpha@extensions.waddle.test", null),
            ),
        )

        val viewModel = harness.viewModel()
        runCurrent()

        val state = viewModel.uiState.value
        assertEquals(listOf("icepuma@waddle.test", "bob@waddle.test"), state.rows.map { it.jid })
        assertEquals(
            listOf(
                BotRow("alpha@extensions.waddle.test", "alpha"),
                BotRow("zeta@extensions.waddle.test", "Zeta Bot"),
            ),
            state.bots,
        )
    }

    @Test
    fun `an extensions-domain address alone is not a bot`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        // Listed as a plain member, absent from the server's bot list.
        harness.client.roomMembersByTier = mapOf(
            WaddleMucAffiliation.MEMBER to listOf(entry("alpha@extensions.waddle.test", WaddleMucAffiliation.MEMBER)),
        )

        val viewModel = harness.viewModel()
        runCurrent()

        assertEquals(listOf("alpha@extensions.waddle.test"), viewModel.uiState.value.rows.map { it.jid })
        assertTrue(viewModel.uiState.value.bots.isEmpty())
    }

    @Test
    fun `a hatted momentary occupant is not listed as a person`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        seedSelfAsOwner(harness)
        harness.factory.emit(botPresence("alpha", "alpha@extensions.waddle.test"))
        runCurrent()

        val viewModel = harness.viewModel()
        runCurrent()
        // Mid-send: present, hatted, affiliation none — still no person row.
        assertEquals(listOf("icepuma"), viewModel.uiState.value.rows.map { it.displayName })

        harness.factory.emit(botPresence("alpha", "alpha@extensions.waddle.test", type = "unavailable"))
        runCurrent()
        assertEquals(listOf("icepuma"), viewModel.uiState.value.rows.map { it.displayName })
    }

    @Test
    fun `a bot-hat presence for an unlisted jid refetches the room bots`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = harness.viewModel()
        runCurrent()
        assertTrue(viewModel.uiState.value.bots.isEmpty())
        val initialFetches = harness.client.listRoomBotsCalls.size

        harness.client.roomBots = mapOf(ROOM to listOf(bot("alpha@extensions.waddle.test", "Alpha")))
        harness.factory.emit(botPresence("alpha", "alpha@extensions.waddle.test"))
        runCurrent()

        assertEquals(initialFetches + 1, harness.client.listRoomBotsCalls.size)
        assertEquals(listOf("Alpha"), viewModel.uiState.value.bots.map { it.displayName })

        // Already listed: the same bot arriving again asks nothing more.
        harness.factory.emit(botPresence("alpha", "alpha@extensions.waddle.test", type = "unavailable"))
        harness.factory.emit(botPresence("alpha", "alpha@extensions.waddle.test"))
        runCurrent()
        assertEquals(initialFetches + 1, harness.client.listRoomBotsCalls.size)
    }

    @Test
    fun `a bot already learned in another room still refetches when it first posts here`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        // alpha is hat-learned session-wide from a different room.
        harness.factory.emit(botPresence("alpha", "alpha@extensions.waddle.test", room = OTHER_ROOM))
        runCurrent()
        val viewModel = harness.viewModel()
        runCurrent()
        val initialFetches = harness.client.listRoomBotsCalls.size

        // Another room's bot presence says nothing about this room's list.
        harness.factory.emit(botPresence("beta", "beta@extensions.waddle.test", room = OTHER_ROOM))
        runCurrent()
        assertEquals(initialFetches, harness.client.listRoomBotsCalls.size)

        // The server records alpha in this room at join; the list names it only now.
        harness.client.roomBots = mapOf(ROOM to listOf(bot("alpha@extensions.waddle.test", "Alpha")))
        harness.factory.emit(botPresence("alpha", "alpha@extensions.waddle.test"))
        runCurrent()

        assertEquals(initialFetches + 1, harness.client.listRoomBotsCalls.size)
        assertEquals(listOf("Alpha"), viewModel.uiState.value.bots.map { it.displayName })
    }

    @Test
    fun `an unavailable bot presence for an unlisted jid refetches the room bots`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = harness.viewModel()
        runCurrent()
        val initialFetches = harness.client.listRoomBotsCalls.size

        // The screen opened mid-send: only the bot's leave is seen.
        harness.client.roomBots = mapOf(ROOM to listOf(bot("alpha@extensions.waddle.test", "Alpha")))
        harness.factory.emit(botPresence("alpha", "alpha@extensions.waddle.test", type = "unavailable"))
        runCurrent()

        assertEquals(initialFetches + 1, harness.client.listRoomBotsCalls.size)
        assertEquals(listOf("Alpha"), viewModel.uiState.value.bots.map { it.displayName })
    }

    @Test
    fun `a bot the list still omits refetches once per load, not per presence`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = harness.viewModel()
        runCurrent()
        val initialFetches = harness.client.listRoomBotsCalls.size

        harness.factory.emit(botPresence("alpha", "alpha@extensions.waddle.test"))
        harness.factory.emit(botPresence("alpha", "alpha@extensions.waddle.test", type = "unavailable"))
        harness.factory.emit(botPresence("alpha", "alpha@extensions.waddle.test"))
        runCurrent()
        assertEquals(initialFetches + 1, harness.client.listRoomBotsCalls.size)

        // A fresh load asks again; the reload itself fetches the bots too.
        viewModel.refresh()
        runCurrent()
        val afterReload = harness.client.listRoomBotsCalls.size
        harness.factory.emit(botPresence("alpha", "alpha@extensions.waddle.test"))
        runCurrent()
        assertEquals(afterReload + 1, harness.client.listRoomBotsCalls.size)
    }

    @Test
    fun `ban and remove map onto outcast and none affiliation sets`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = harness.viewModel()
        runCurrent()
        val row = MemberRow(
            jid = "mallory@waddle.test",
            displayName = "mallory",
            affiliation = WaddleMucAffiliation.MEMBER,
            nick = null,
            presentNow = false,
            hats = emptyList(),
            inferred = false,
        )

        viewModel.ban(row)
        runCurrent()
        viewModel.remove(row)
        runCurrent()

        val affiliations = harness.client.setAffiliationCalls.map { it[2] }
        assertEquals(
            listOf<Any?>(WaddleMucAffiliation.OUTCAST, WaddleMucAffiliation.NONE),
            affiliations,
        )
    }

    @Test
    fun `kick addresses the occupant nick`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = harness.viewModel()
        runCurrent()
        val row = MemberRow(
            jid = "mallory@waddle.test",
            displayName = "mallory",
            affiliation = WaddleMucAffiliation.MEMBER,
            nick = "mallory",
            presentNow = true,
            hats = emptyList(),
            inferred = false,
        )

        viewModel.kick(row)
        runCurrent()

        assertEquals(ROOM, harness.client.kickCalls.single().first)
        assertEquals("mallory", harness.client.kickCalls.single().second)
    }

    @Test
    fun `search debounces, filters existing members, and adds as member`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.roomMembersByTier = mapOf(
            WaddleMucAffiliation.MEMBER to listOf(entry("bob@waddle.test", WaddleMucAffiliation.MEMBER)),
        )
        seedSelfAsOwner(harness)
        runCurrent()
        val viewModel = harness.viewModel()
        runCurrent()
        harness.client.directory.userSearchResults = listOf(
            WaddleUserSearchEntry(jid = "bob@waddle.test", username = "bob", displayName = null),
            WaddleUserSearchEntry(jid = "carol@waddle.test", username = "carol", displayName = "Carol"),
        )

        viewModel.onSearchQueryChanged("caro")
        runCurrent()
        assertTrue(harness.client.directory.searchUsersCalls.isEmpty())
        advanceTimeBy(250)
        runCurrent()

        assertEquals(listOf("caro"), harness.client.directory.searchUsersCalls.toList())
        // bob is already a member — filtered out.
        assertEquals(
            listOf("carol@waddle.test"),
            viewModel.uiState.value.searchResults.map { it.jid },
        )

        viewModel.addMember(viewModel.uiState.value.searchResults.single())
        runCurrent()

        assertEquals(
            listOf<Any?>(ROOM, "carol@waddle.test", WaddleMucAffiliation.MEMBER, null),
            harness.client.setAffiliationCalls.single(),
        )
        assertEquals("", viewModel.uiState.value.searchQuery)
    }

    private companion object {
        const val ROOM = "general@muc.waddle.test"
        const val OTHER_ROOM = "random@muc.waddle.test"
    }
}
