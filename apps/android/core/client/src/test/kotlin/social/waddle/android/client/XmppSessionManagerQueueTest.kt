package social.waddle.android.client

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.emptyPreferences
import app.cash.turbine.test
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.session.ActiveSession
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleSendMessageOutcome
import java.io.IOException

/** Durable outbound intent behavior through the Android session manager. */
@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionManagerQueueTest {
    private class FailingPreferencesDataStore : DataStore<Preferences> {
        override val data = flowOf(emptyPreferences())

        override suspend fun updateData(
            transform: suspend (t: Preferences) -> Preferences,
        ): Preferences = throw IOException("disk full")
    }

    /**
     * Stops exactly one DataStore update after it commits the outbound row
     * but before it returns to the caller. This lets the test force logout
     * through the post-persistence lease check deterministically.
     */
    private class BlockingPreferencesDataStore : DataStore<Preferences> {
        private val mutex = Mutex()
        private val state = MutableStateFlow<Preferences>(emptyPreferences())
        val enqueueBlocked = CompletableDeferred<Unit>()
        val releaseEnqueue = CompletableDeferred<Unit>()
        var blockNextUpdate = false

        override val data = state

        override suspend fun updateData(
            transform: suspend (t: Preferences) -> Preferences,
        ): Preferences = mutex.withLock {
            val next = transform(state.value)
            state.value = next
            if (blockNextUpdate) {
                blockNextUpdate = false
                enqueueBlocked.complete(Unit)
                releaseEnqueue.await()
            }
            next
        }
    }

    private class Harness(
        testScope: TestScope,
        val prefs: SessionPrefs = SessionPrefs(InMemoryPreferencesDataStore()),
    ) {
        val factory = FakeClientFactory()
        val network = FakeNetworkSignal()
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
    fun `offline send persists the typed intent and returns its stable id`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        val result = harness.manager.sendChatMessage("alice@waddle.test", "hi from the subway")

        assertEquals(WaddleSendMessageOutcome.NotConnected, result.outcome)
        assertNotNull("durable id hands replay identity to the caller", result.queuedId)
        val queued = harness.prefs.outboundQueue.first().single()
        assertEquals("icepuma@waddle.test", queued.ownerBareJid)
        assertEquals("alice@waddle.test", queued.conversationJid)
        assertFalse(queued.isGroupchat)
        assertEquals("hi from the subway", queued.body)
        assertEquals(result.queuedId, queued.clientStanzaId)

        harness.manager.logout()
    }

    @Test
    fun `online Sent remains durable until matching ack which commits before dispatch`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        harness.manager.events.test {
            val result = harness.manager.sendChatMessage("alice@waddle.test", "online")
            val id = result.queuedId!!
            assertEquals(WaddleSendMessageOutcome.Sent(id), result.outcome)
            assertEquals(id, harness.prefs.outboundQueue.first().single().clientStanzaId)

            harness.factory.emit(WaddleClientEvent.DeliveryAcked(id))
            runCurrent()

            assertTrue("ack deletion precedes router/UI dispatch", harness.prefs.outboundQueue.first().isEmpty())
            assertEquals(XmppEvent.DeliveryAcked(id), awaitItem())
        }

        harness.manager.logout()
    }

    @Test
    fun `unrelated ack and runtime delivery failure retain the exact row`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val result = harness.manager.sendGroupchatMessage("general@muc.waddle.test", "keep")
        val id = result.queuedId!!

        harness.factory.emit(WaddleClientEvent.DeliveryAcked("another-id"))
        runCurrent()
        assertEquals(id, harness.prefs.outboundQueue.first().single().clientStanzaId)

        harness.factory.emit(WaddleClientEvent.DeliveryFailed(id))
        runCurrent()
        assertEquals(
            "runtime failure is not proof of a terminal durable negative receipt",
            id,
            harness.prefs.outboundQueue.first().single().clientStanzaId,
        )

        harness.manager.logout()
    }

    @Test
    fun `ack after replay wins serialization only after the in-flight send`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val client = harness.factory.clients.single()
        val releaseSend = CompletableDeferred<Unit>()
        client.sendMessageStall = releaseSend
        val send = async { harness.manager.sendChatMessage("alice@waddle.test", "racy") }
        runCurrent()

        val id = harness.prefs.outboundQueue.first().single().clientStanzaId
        assertEquals(id, client.sendOptions.single()?.stanzaId)
        harness.factory.emit(WaddleClientEvent.DeliveryAcked(id))
        runCurrent()
        assertEquals(
            "the replay already owns sendMutex, so the ack waits rather than racing its durable selection",
            id,
            harness.prefs.outboundQueue.first().single().clientStanzaId,
        )

        releaseSend.complete(Unit)
        runCurrent()
        assertEquals(id, send.await().queuedId)
        assertTrue("the queued ack removes the exact row after the send linearizes", harness.prefs.outboundQueue.first().isEmpty())

        harness.manager.logout()
    }

    @Test
    fun `ack before fresh-ready replay suppresses the retained durable intent`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        val queued = harness.manager.sendChatMessage("alice@waddle.test", "ack wins before replay")
        val id = queued.queuedId!!
        assertEquals(id, harness.prefs.outboundQueue.first().single().clientStanzaId)

        // This is the old contains/send interleaving boundary. There is no
        // longer a race window: acknowledgement acquires sendMutex first,
        // deletes the exact durable intent, and the ready drain has nothing
        // eligible to send.
        harness.factory.emit(WaddleClientEvent.DeliveryAcked(id))
        runCurrent()
        assertTrue(harness.prefs.outboundQueue.first().isEmpty())

        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        assertTrue("an ack that linearizes first suppresses stale replay", harness.factory.clients.single().sendCalls.isEmpty())

        harness.manager.logout()
    }

    @Test
    fun `fresh ready replays retained rows once in order with the same origin ids`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        val first = harness.manager.sendChatMessage("alice@waddle.test", "one")
        val second = harness.manager.sendGroupchatMessage("general@muc.waddle.test", "two")

        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val client = harness.factory.clients.single()
        assertEquals(
            listOf("alice@waddle.test" to "one", "general@muc.waddle.test" to "two"),
            client.sendCalls,
        )
        assertEquals(listOf(first.queuedId, second.queuedId), client.sendOptions.map { it?.stanzaId })
        assertEquals(
            "transport acceptance retains both rows",
            listOf(first.queuedId, second.queuedId),
            harness.prefs.outboundQueue.first().map { it.clientStanzaId },
        )

        harness.factory.emit(WaddleClientEvent.DeliveryAcked(first.queuedId!!))
        harness.factory.emit(WaddleClientEvent.DeliveryAcked(second.queuedId!!))
        runCurrent()
        assertTrue(harness.prefs.outboundQueue.first().isEmpty())

        harness.manager.logout()
    }

    @Test
    fun `transport failure keeps the queue head and order`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        harness.manager.sendChatMessage("alice@waddle.test", "one")
        harness.manager.sendChatMessage("alice@waddle.test", "two")

        val client = harness.factory.clients.single()
        client.sendOutcome = WaddleSendMessageOutcome.TransportError
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        assertEquals(listOf("alice@waddle.test" to "one"), client.sendCalls)
        assertEquals(listOf("one", "two"), harness.prefs.outboundQueue.first().map { it.body })

        harness.manager.logout()
    }

    @Test
    fun `permanent synchronous replay rejection deletes only the exact row`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        val doomed = harness.manager.sendChatMessage("nobody@waddle.test", "doomed")
        val retained = harness.manager.sendChatMessage("alice@waddle.test", "fine")

        val client = harness.factory.clients.single()
        client.sendOutcomes += WaddleSendMessageOutcome.InvalidRecipient
        client.sendOutcomes += WaddleSendMessageOutcome.Sent("ignored-by-manager")

        harness.manager.events.test {
            harness.factory.emit(WaddleClientEvent.Connected)
            runCurrent()

            assertEquals(XmppEvent.SessionReady, awaitItem())
            assertEquals(XmppEvent.DeliveryFailed(doomed.queuedId!!), awaitItem())
            assertTrue(awaitItem() is XmppEvent.Error)
        }

        assertEquals(2, client.sendCalls.size)
        assertEquals(
            listOf(retained.queuedId),
            harness.prefs.outboundQueue.first().map { it.clientStanzaId },
        )

        harness.manager.logout()
    }

    @Test
    fun `full queue rejects the new intent without evicting accepted rows`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        repeat(OutboundQueue.DEFAULT_CAPACITY) { index ->
            assertTrue(harness.manager.sendChatMessage("alice@waddle.test", "m-$index").queued)
        }
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        val sendCountBeforeOverflow = harness.factory.clients.single().sendCalls.size
        val accepted = harness.prefs.outboundQueue.first()

        val overflow = harness.manager.sendChatMessage("alice@waddle.test", "m-overflow")

        assertEquals(WaddleSendMessageOutcome.Error, overflow.outcome)
        assertNull(overflow.queuedId)
        assertEquals(
            "capacity rejection occurs before the FFI transport",
            sendCountBeforeOverflow,
            harness.factory.clients.single().sendCalls.size,
        )
        assertEquals(accepted, harness.prefs.outboundQueue.first())

        harness.manager.logout()
    }

    @Test
    fun `persistence failure returns typed Error before transport`() = runTest {
        val harness = Harness(
            testScope = this,
            prefs = SessionPrefs(FailingPreferencesDataStore()),
        )
        harness.manager.login(testSessionInfo())
        runCurrent()

        val result = harness.manager.sendChatMessage("alice@waddle.test", "must persist")

        assertEquals(WaddleSendMessageOutcome.Error, result.outcome)
        assertNull(result.queuedId)
        assertTrue("no client transport was created or called", harness.factory.clients.isEmpty())

        runCatching { harness.manager.logout() }
    }

    @Test
    fun `blocked enqueue is fenced across logout and same-account relogin`() = runTest {
        val store = BlockingPreferencesDataStore()
        val harness = Harness(this, SessionPrefs(store))
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        val oldClient = harness.factory.clients.single()

        store.blockNextUpdate = true
        val staleSend = async {
            harness.manager.sendChatMessage("alice@waddle.test", "must not cross logout")
        }
        store.enqueueBlocked.await()

        // logout advances the generation before waiting on the blocked
        // DataStore clear. The parked enqueue may finish, but its exact
        // owner/id is removed and it is never handed to the old transport.
        val logout = async { harness.manager.logout() }
        runCurrent()
        store.releaseEnqueue.complete(Unit)
        runCurrent()

        assertEquals(WaddleSendMessageOutcome.Error, staleSend.await().outcome)
        logout.await()
        assertTrue(harness.prefs.outboundQueue.first().isEmpty())
        assertTrue("stale intent never reaches the pre-logout transport", oldClient.sendCalls.isEmpty())

        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        assertTrue(
            "same-account relogin is a new generation and cannot replay stale work",
            harness.factory.clients.last().sendCalls.isEmpty(),
        )

        harness.manager.logout()
    }

    @Test
    fun `an old same-account lease cannot invoke the relogin client`() = runTest {
        val activeSession = ActiveSession()
        val oldClient = FakeWaddleClient()
        val successorClient = FakeWaddleClient()
        activeSession.advanceGeneration()
        activeSession.activateOwner("icepuma@waddle.test")
        activeSession.onReady(oldClient)
        val oldLease = checkNotNull(activeSession.captureOwnerLease())

        // Logout and a login to the same bare JID replace the attempt. The
        // old lease must not select the newly-ready FFI client.
        activeSession.advanceGeneration()
        activeSession.endAttempt(oldClient)
        activeSession.activateOwner("icepuma@waddle.test")
        activeSession.onReady(successorClient)

        val result = activeSession.sendIfCurrent(oldLease) { client ->
            client.sendChatMessage("alice@waddle.test", "must stay fenced", null)
        }

        assertEquals(ActiveSession.LeaseSendResult.Stale, result)
        assertTrue("the old lease cannot use its retired client", oldClient.sendCalls.isEmpty())
        assertTrue("the old lease cannot switch to the relogin client", successorClient.sendCalls.isEmpty())
    }

    @Test
    fun `transport use and logout linearize at the active-session fence`() = runTest {
        val activeSession = ActiveSession()
        val oldClient = FakeWaddleClient()
        activeSession.advanceGeneration()
        activeSession.activateOwner("icepuma@waddle.test")
        activeSession.onReady(oldClient)
        val lease = checkNotNull(activeSession.captureOwnerLease())

        // This pause is after the lease check and client selection. The send
        // now owns the transport fence, so logout cannot retire that client
        // midway through its FFI call.
        val sendEntered = CompletableDeferred<Unit>()
        val releaseSend = CompletableDeferred<Unit>()
        val send = async {
            activeSession.sendIfCurrent(lease) { client ->
                sendEntered.complete(Unit)
                releaseSend.await()
                client.sendChatMessage("alice@waddle.test", "send wins", null)
            }
        }
        runCurrent()
        sendEntered.await()
        val logout = async { activeSession.revokeOutboundAuthority() }
        runCurrent()
        assertFalse("logout waits for an already-authorized send", logout.isCompleted)

        releaseSend.complete(Unit)
        assertTrue(send.await() is ActiveSession.LeaseSendResult.Attempted)
        logout.await()
        assertEquals(listOf("alice@waddle.test" to "send wins"), oldClient.sendCalls)

        // Conversely, once revocation owns the fence, an old lease cannot
        // select the retired client at all.
        val afterRevoke = activeSession.sendIfCurrent(lease) { client ->
            client.sendChatMessage("alice@waddle.test", "logout wins", null)
        }
        assertEquals(ActiveSession.LeaseSendResult.Stale, afterRevoke)
        assertEquals(listOf("alice@waddle.test" to "send wins"), oldClient.sendCalls)
    }

    @Test
    fun `queue cleanup is scoped to the exact owner and stanza id`() = runTest {
        val harness = Harness(this)
        harness.prefs.updateOutboundQueue {
            listOf(
                QueuedOutboundMessage(
                    ownerBareJid = "alice@waddle.test",
                    conversationJid = "peer@waddle.test",
                    isGroupchat = false,
                    body = "alice only",
                    clientStanzaId = "shared-id",
                    enqueuedAtMillis = 0L,
                ),
                QueuedOutboundMessage(
                    ownerBareJid = "bob@waddle.test",
                    conversationJid = "peer@waddle.test",
                    isGroupchat = false,
                    body = "bob survives",
                    clientStanzaId = "shared-id",
                    enqueuedAtMillis = 0L,
                ),
            )
        }

        OutboundQueue(harness.prefs).remove("alice@waddle.test", "shared-id")

        assertEquals(
            listOf("bob@waddle.test" to "shared-id"),
            harness.prefs.outboundQueue.first().map { it.ownerBareJid to it.clientStanzaId },
        )
    }

    @Test
    fun `initial permanent rejection removes its exact persisted row before failure dispatch`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        harness.factory.clients.single().sendOutcome = WaddleSendMessageOutcome.InvalidRecipient

        harness.manager.events.test {
            val result = harness.manager.sendChatMessage("nobody@waddle.test", "rejected")

            assertEquals(WaddleSendMessageOutcome.InvalidRecipient, result.outcome)
            assertNull(result.queuedId)
            assertTrue(harness.prefs.outboundQueue.first().isEmpty())
            assertTrue(awaitItem() is XmppEvent.DeliveryFailed)
            assertTrue(awaitItem() is XmppEvent.Error)
        }

        harness.manager.logout()
    }

    @Test
    fun `crash after Sent replays the same origin id in a new manager`() = runTest {
        val firstProcess = Harness(this)
        firstProcess.manager.login(testSessionInfo())
        runCurrent()
        firstProcess.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val sent = firstProcess.manager.sendChatMessage("alice@waddle.test", "survive crash")
        val id = sent.queuedId!!
        assertEquals(id, firstProcess.prefs.outboundQueue.first().single().clientStanzaId)

        val secondProcess = Harness(this, firstProcess.prefs)
        secondProcess.manager.login(testSessionInfo())
        runCurrent()
        secondProcess.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val replayClient = secondProcess.factory.clients.single()
        assertEquals(listOf("alice@waddle.test" to "survive crash"), replayClient.sendCalls)
        assertEquals(id, replayClient.sendOptions.single()?.stanzaId)

        secondProcess.factory.emit(WaddleClientEvent.DeliveryAcked(id))
        runCurrent()
        assertTrue(secondProcess.prefs.outboundQueue.first().isEmpty())

        secondProcess.manager.logout()
        firstProcess.manager.logout()
    }

    @Test
    fun `queued sticker replay preserves its full typed wire shape`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        val result = harness.manager.sendChatMessage(
            peerJid = "alice@waddle.test",
            body = "🐧",
            extras = MessageSendExtras(
                sticker = StickerSendRef(
                    packId = "pack-1",
                    desc = "🐧",
                    url = "https://upload.waddle.test/penguin.webp",
                    mediaType = "image/webp",
                    hashes = listOf(StickerHash(algo = "sha-256", valueB64 = "aGFzaA==")),
                ),
            ),
        )

        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val options = harness.factory.clients.single().sendOptions.single()
        assertEquals("pack-1", options?.sticker?.packId)
        assertEquals("🐧", options?.sharedFiles?.single()?.desc)
        assertEquals("image/webp", options?.sharedFiles?.single()?.mediaType)
        assertEquals("aGFzaA==", options?.sharedFiles?.single()?.hashes?.single()?.valueB64)
        assertEquals(result.queuedId, options?.stanzaId)

        harness.manager.logout()
    }

    @Test
    fun `another account's rows are pruned and never replayed`() = runTest {
        val harness = Harness(this)
        harness.prefs.updateOutboundQueue {
            listOf(
                QueuedOutboundMessage(
                    ownerBareJid = "someone-else@waddle.test",
                    conversationJid = "peer@waddle.test",
                    isGroupchat = false,
                    body = "secret written by the previous account",
                    clientStanzaId = "q-foreign",
                    enqueuedAtMillis = 0L,
                ),
            )
        }

        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        assertTrue(harness.factory.clients.single().sendCalls.none { it.second.contains("secret") })
        assertEquals(emptyList<QueuedOutboundMessage>(), harness.prefs.outboundQueue.first())

        harness.manager.logout()
    }
}
