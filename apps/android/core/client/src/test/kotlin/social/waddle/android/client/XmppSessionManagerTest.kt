package social.waddle.android.client

import app.cash.turbine.test
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleMamPage
import social.waddle.client.ffi.WaddleSaslCondition
import social.waddle.client.ffi.WaddleSendMessageOutcome

@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionManagerTest {
    private class Harness(testScope: TestScope) {
        val factory = FakeClientFactory()
        val network = FakeNetworkSignal()
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val manager = XmppSessionManager(
            sessionPrefs = prefs,
            clientFactory = factory,
            networkSignal = network,
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScope.testScheduler),
        )
    }

    @Test
    fun `login goes connecting then ready on session ready`() = runTest {
        val harness = Harness(this)

        harness.manager.connectionState.test {
            assertEquals(ConnectionState.Idle, awaitItem())

            harness.manager.login(testSessionInfo())
            runCurrent()
            assertEquals(ConnectionState.Connecting, awaitItem())

            harness.factory.emit(WaddleClientEvent.Connected)
            runCurrent()
            assertEquals(ConnectionState.Ready, awaitItem())
        }

        assertEquals(WaddleAppState.Ready, harness.manager.appState.value)
        assertEquals("sess-1", harness.prefs.sessionId.first())
        assertEquals(1, harness.factory.clients.size)

        val config = harness.factory.configs.single()
        assertEquals("wss://waddle.test/xmpp", config.serverUrl)
        assertEquals("icepuma@waddle.test", config.jid)
        assertEquals("sess-1", config.accessToken)
        assertTrue(
            "resource ${config.resource} must be waddle-android-<8hex>",
            config.resource.matches(Regex("waddle-android-[0-9a-f]{8}")),
        )

        harness.manager.logout()
    }

    @Test
    fun `session drop retries with the backoff policy`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        assertEquals(ConnectionState.Ready, harness.manager.connectionState.value)

        harness.factory.emit(WaddleClientEvent.Error("websocket stream closed"))
        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()

        // Attempt counter reset on ready → first retry uses attempt 0.
        assertEquals(
            ConnectionState.Reconnecting(attempt = 1, nextDelayMs = 1_000L),
            harness.manager.connectionState.value,
        )
        assertEquals(1, harness.factory.clients.size)

        advanceTimeBy(1_000L)
        runCurrent()
        assertEquals(ConnectionState.Connecting, harness.manager.connectionState.value)
        assertEquals("a fresh client per attempt", 2, harness.factory.clients.size)
        assertEquals(1, harness.factory.clients[0].disconnectCalls)

        harness.manager.logout()
    }

    @Test
    fun `attempt budget exhaustion is terminal`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        repeat(ReconnectPolicy.MAX_ATTEMPTS) { attempt ->
            harness.factory.emit(WaddleClientEvent.Disconnected)
            runCurrent()
            val state = harness.manager.connectionState.value
            assertTrue(
                "expected Reconnecting after failure $attempt, got $state",
                state is ConnectionState.Reconnecting,
            )
            assertEquals(attempt + 1, (state as ConnectionState.Reconnecting).attempt)
            advanceTimeBy(state.nextDelayMs)
            runCurrent()
        }

        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        assertEquals(ConnectionState.Failed, harness.manager.connectionState.value)
        assertEquals("app stays signed in on transport failure", WaddleAppState.Ready, harness.manager.appState.value)

        // Failed is parked, not terminal: a manual retry restarts the
        // loop with a fresh budget (web connectWithFreshBudget parity).
        val clientsBefore = harness.factory.clients.size
        harness.manager.requestReconnect()
        runCurrent()
        assertTrue(
            "retry restarts the connection loop",
            harness.factory.clients.size > clientsBefore,
        )

        harness.manager.logout()
    }

    @Test
    fun `budget exhaustion recovers on an offline-online edge`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        repeat(ReconnectPolicy.MAX_ATTEMPTS + 1) {
            harness.factory.emit(WaddleClientEvent.Disconnected)
            runCurrent()
            (harness.manager.connectionState.value as? ConnectionState.Reconnecting)?.let {
                advanceTimeBy(it.nextDelayMs)
                runCurrent()
            }
        }
        assertEquals(ConnectionState.Failed, harness.manager.connectionState.value)

        val clientsBefore = harness.factory.clients.size
        harness.network.state.value = false
        runCurrent()
        harness.network.state.value = true
        runCurrent()
        assertTrue(
            "connectivity return restarts the loop",
            harness.factory.clients.size > clientsBefore,
        )

        harness.manager.logout()
    }

    @Test
    fun `auth shaped error is terminal and signs out`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        harness.factory.emit(
            WaddleClientEvent.AuthenticationFailed(WaddleSaslCondition.NOT_AUTHORIZED),
        )
        runCurrent()

        assertEquals(ConnectionState.AuthFailed, harness.manager.connectionState.value)
        assertEquals(WaddleAppState.SignedOut, harness.manager.appState.value)
        assertEquals("session id cleared on auth failure", null, harness.prefs.sessionId.first())
        assertEquals("no retry after auth failure", 1, harness.factory.clients.size)
    }

    @Test
    fun `temporary auth failure retries instead of signing out`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        harness.factory.emit(
            WaddleClientEvent.AuthenticationFailed(WaddleSaslCondition.TEMPORARY_AUTH_FAILURE),
        )
        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()

        // RFC 6120 §6.5: temporary-auth-failure is transient — the session
        // survives and the loop backs off instead of wiping prefs.
        assertEquals(WaddleAppState.Ready, harness.manager.appState.value)
        assertNotNull(harness.prefs.sessionId.first())
        assertTrue(harness.manager.connectionState.value is ConnectionState.Reconnecting)

        harness.manager.logout()
    }

    @Test
    fun `auth shaped error after session ready reconnects instead of signing out`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        assertEquals(ConnectionState.Ready, harness.manager.connectionState.value)

        // Post-ready, the same "forbidden"/"not-authorized" shaped text
        // arrives on per-operation stanza errors — it must never wipe the
        // session (only pre-ready classification is terminal).
        harness.factory.emit(WaddleClientEvent.Error("stanza error: forbidden"))
        runCurrent()

        assertEquals(WaddleAppState.Ready, harness.manager.appState.value)
        assertNotNull(harness.prefs.sessionId.first())

        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        assertTrue(
            "drops back to reconnect, not sign-out",
            harness.manager.connectionState.value is ConnectionState.Reconnecting ||
                harness.manager.connectionState.value is ConnectionState.Connecting,
        )

        harness.manager.logout()
    }

    @Test
    fun `persisted rooms are rejoined on every fresh session`() = runTest {
        val harness = Harness(this)
        harness.prefs.setJoinedRooms(setOf("general@muc.waddle.test", "dev@muc.waddle.test"))

        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val client = harness.factory.clients.last()
        assertEquals(
            setOf("general@muc.waddle.test", "dev@muc.waddle.test"),
            client.joinRoomCalls.map { it.first }.toSet(),
        )
        assertEquals(
            "nick is the session localpart",
            testSessionInfo().xmppLocalpart,
            client.joinRoomCalls.first().second,
        )

        harness.manager.logout()
    }

    @Test
    fun `offline network parks the loop until connectivity returns`() = runTest {
        val harness = Harness(this)
        harness.network.state.value = false

        harness.manager.login(testSessionInfo())
        runCurrent()

        assertEquals(ConnectionState.Offline, harness.manager.connectionState.value)
        assertEquals("no attempt while offline", 0, harness.factory.clients.size)

        harness.network.state.value = true
        runCurrent()
        assertEquals(ConnectionState.Connecting, harness.manager.connectionState.value)
        assertEquals(1, harness.factory.clients.size)

        harness.manager.logout()
    }

    @Test
    fun `connect timeout burns the attempt`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        assertEquals(ConnectionState.Connecting, harness.manager.connectionState.value)

        advanceTimeBy(XmppSessionManager.CONNECT_TIMEOUT_MILLIS)
        runCurrent()

        assertEquals(
            ConnectionState.Reconnecting(attempt = 1, nextDelayMs = 1_000L),
            harness.manager.connectionState.value,
        )

        harness.manager.logout()
    }

    @Test
    fun `live events fan out to stores and the shared flow`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        harness.manager.events.test {
            harness.factory.emit(WaddleClientEvent.Connected)
            runCurrent()
            assertEquals(XmppEvent.SessionReady, awaitItem())

            val message = testMessage(stanzaId = "s1", from = "alice@waddle.test", to = "icepuma@waddle.test")
            harness.factory.emit(WaddleClientEvent.Message(message))
            runCurrent()
            assertEquals(XmppEvent.Message(message), awaitItem())
        }

        assertEquals(1, harness.manager.timelineStore.timeline("alice@waddle.test").value.size)
        assertEquals(listOf("alice@waddle.test"), harness.manager.dmStore.peers.value)
        assertEquals(mapOf("alice@waddle.test" to 1), harness.manager.unreadStore.counts.value)

        harness.manager.logout()
    }

    @Test
    fun `passthroughs report not connected before session ready`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        assertEquals(VerbResult.NotConnected, harness.manager.joinRoom("general@muc.waddle.test", "icepuma"))
        assertEquals(null, harness.manager.fetchRoomHistory("general@muc.waddle.test", 50u, null))
        val roomSend = harness.manager.sendGroupchatMessage("general@muc.waddle.test", "hi")
        assertEquals(WaddleSendMessageOutcome.NotConnected, roomSend.outcome)
        assertTrue("session-shaped failure queues for replay", roomSend.queued)
        val dmSend = harness.manager.sendChatMessage("alice@waddle.test", "hi")
        assertEquals(WaddleSendMessageOutcome.NotConnected, dmSend.outcome)
        assertTrue(dmSend.queued)
        assertTrue(harness.factory.clients.single().sendCalls.isEmpty())

        harness.manager.logout()
    }

    @Test
    fun `join passthrough marks and persists the joined room`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        assertEquals(VerbResult.Ok, harness.manager.joinRoom("general@muc.waddle.test", "icepuma"))
        val client = harness.factory.clients.single()
        assertEquals(listOf("general@muc.waddle.test" to "icepuma"), client.joinRoomCalls)
        assertEquals(setOf("general@muc.waddle.test"), harness.manager.roomStore.joinedRooms.value)
        assertEquals(setOf("general@muc.waddle.test"), harness.prefs.joinedRooms.first())

        client.joinRoomFailure = IllegalStateException("boom")
        assertEquals(VerbResult.Rejected, harness.manager.joinRoom("other@muc.waddle.test", "icepuma"))
        assertEquals(setOf("general@muc.waddle.test"), harness.manager.roomStore.joinedRooms.value)

        harness.manager.logout()
    }

    @Test
    fun `history passthrough fans the page into the timeline store`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val client = harness.factory.clients.single()
        client.mamPage = WaddleMamPage(
            messages = listOf(
                testArchivedMessage(
                    mamId = "mam-1",
                    stanzaId = "s1",
                    from = "general@muc.waddle.test/alice",
                    to = "icepuma@waddle.test",
                    messageType = "groupchat",
                ),
            ),
            firstId = "mam-1",
            lastId = "mam-1",
            isComplete = false,
        )

        val page = harness.manager.fetchRoomHistory("general@muc.waddle.test", 50u, "mam-9")
        assertEquals("mam-1", page?.firstId)
        assertEquals(
            listOf(Triple("general@muc.waddle.test", 50u, "mam-9")),
            client.fetchHistoryCalls,
        )
        assertEquals(
            1,
            harness.manager.timelineStore.timeline("general@muc.waddle.test").value.size,
        )

        harness.manager.logout()
    }

    @Test
    fun `send passthroughs delegate to the live client`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val client = harness.factory.clients.single()
        client.sendOutcome = WaddleSendMessageOutcome.Sent("stanza-42")

        val roomSend = harness.manager.sendGroupchatMessage("general@muc.waddle.test", "hello room")
        val dmSend = harness.manager.sendChatMessage("alice@waddle.test", "hello dm")
        assertEquals(WaddleSendMessageOutcome.Sent(roomSend.queuedId!!), roomSend.outcome)
        assertEquals(WaddleSendMessageOutcome.Sent(dmSend.queuedId!!), dmSend.outcome)
        assertTrue("live sends remain durable until SM acknowledgement", roomSend.queued && dmSend.queued)
        assertEquals(
            listOf("general@muc.waddle.test" to "hello room", "alice@waddle.test" to "hello dm"),
            client.sendCalls,
        )
        assertTrue(
            "manager-generated stanza id rides the send options",
            client.sendOptions.all { it?.stanzaId != null },
        )
        assertEquals(
            listOf(roomSend.queuedId, dmSend.queuedId),
            harness.prefs.outboundQueue.first().map { it.clientStanzaId },
        )

        // The attempt died → the passthrough must stop targeting the client.
        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        assertEquals(
            WaddleSendMessageOutcome.NotConnected,
            harness.manager.sendChatMessage("alice@waddle.test", "late").outcome,
        )

        harness.manager.logout()
    }

    @Test
    fun `logout tears an active call down before the stream closes`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        // Drive the slot to Active: our propose, peer accepts.
        harness.manager.callStore.startCall(
            peerJid = "bob@waddle.test",
            media = social.waddle.client.ffi.WaddleCallMedia(audio = true, video = false),
        )
        runCurrent()
        val sid = (harness.manager.callStore.state.value as social.waddle.android.client.calls.CallState.Outgoing).sid
        harness.factory.emit(
            WaddleClientEvent.Call(
                social.waddle.client.ffi.WaddleCallEvent(
                    from = "bob@waddle.test/phone",
                    to = null,
                    sid = sid,
                    kind = social.waddle.client.ffi.WaddleCallEventKind.SessionAccept(
                        join = social.waddle.client.ffi.WaddleLiveKitJoin(
                            url = "wss://livekit.waddle.test",
                            room = "dm-room",
                            identity = "icepuma@waddle.test/r",
                            token = "jwt",
                        ),
                        media = social.waddle.client.ffi.WaddleCallMedia(audio = true, video = false),
                    ),
                ),
            ),
        )
        runCurrent()
        assertTrue(
            harness.manager.callStore.state.value is social.waddle.android.client.calls.CallState.Active,
        )

        harness.manager.logout()
        runCurrent()

        // Web client.ts disconnect parity: terminate + <finish/> went
        // out BEFORE the session died, and the slot reset.
        val client = harness.factory.clients.single()
        assertTrue(client.callVerbs.any { it is RecordedCallVerb.SessionTerminateWithOutcome })
        assertTrue(client.callVerbs.any { it is RecordedCallVerb.Finish })
        assertEquals(
            social.waddle.android.client.calls.CallState.Idle,
            harness.manager.callStore.state.value,
        )
    }

    @Test
    fun `logout call teardown is bounded on a dead network`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        harness.manager.callStore.startCall(
            peerJid = "bob@waddle.test",
            media = social.waddle.client.ffi.WaddleCallMedia(audio = true, video = false),
        )
        runCurrent()
        val sid = (
            harness.manager.callStore.state.value as social.waddle.android.client.calls.CallState.Outgoing
            ).sid
        harness.factory.emit(
            WaddleClientEvent.Call(
                social.waddle.client.ffi.WaddleCallEvent(
                    from = "bob@waddle.test/phone",
                    to = null,
                    sid = sid,
                    kind = social.waddle.client.ffi.WaddleCallEventKind.SessionAccept(
                        join = social.waddle.client.ffi.WaddleLiveKitJoin(
                            url = "wss://livekit.waddle.test",
                            room = "dm-room",
                            identity = "icepuma@waddle.test/r",
                            token = "jwt",
                        ),
                        media = social.waddle.client.ffi.WaddleCallMedia(audio = true, video = false),
                    ),
                ),
            ),
        )
        runCurrent()

        // A silently-dead network: the terminate IQ would only fail
        // after the FFI's 30 s timeout. Sign-out must not wait for it.
        harness.factory.clients.single().callTerminateDelayMillis = 60_000L
        val before = testScheduler.currentTime
        harness.manager.logout()
        runCurrent()

        assertTrue(
            testScheduler.currentTime - before <= XmppSessionManager.LOGOUT_CALL_TEARDOWN_MILLIS,
        )
        assertEquals(WaddleAppState.SignedOut, harness.manager.appState.value)
    }

    @Test
    fun `logout clears state and stores`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        harness.factory.emit(WaddleClientEvent.Message(testMessage(stanzaId = "s1")))
        runCurrent()

        harness.manager.logout()

        assertEquals(ConnectionState.Idle, harness.manager.connectionState.value)
        assertEquals(WaddleAppState.SignedOut, harness.manager.appState.value)
        assertEquals(null, harness.prefs.sessionId.first())
        assertTrue(harness.manager.timelineStore.timeline("alice@waddle.test").value.isEmpty())
        assertEquals(1, harness.factory.clients.single().disconnectCalls)
    }
}
