package social.waddle.android.client

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.store.PeopleDirectorySnapshot
import social.waddle.android.client.store.PresenceShow
import social.waddle.android.client.store.RoomHeadcount
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleMdsDisplayedEntry
import social.waddle.client.ffi.WaddleMucRole
import social.waddle.client.ffi.WaddleMujiPresence

/**
 * Presence, mentions and the People projection through the session
 * manager: what one stream reported never outlives it, mentions of the
 * account count with their unread, and the projection follows both.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionManagerPresenceStoreTest {
    private class Harness(private val scope: TestScope) {
        val factory = FakeClientFactory()
        val manager = XmppSessionManager(
            sessionPrefs = SessionPrefs(InMemoryPreferencesDataStore()),
            clientFactory = factory,
            networkSignal = FakeNetworkSignal(),
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(scope.testScheduler),
        )

        suspend fun loginReady() {
            manager.login(testSessionInfo())
            scope.runCurrent()
            factory.emit(WaddleClientEvent.Connected)
            scope.runCurrent()
        }

        fun emit(event: WaddleClientEvent) {
            factory.emit(event)
            scope.runCurrent()
        }

        fun reconnect() {
            emit(WaddleClientEvent.Disconnected)
            scope.advanceTimeBy(RECONNECT_DELAY_MILLIS)
            scope.runCurrent()
            emit(WaddleClientEvent.Connected)
        }
    }

    private fun occupant(nick: String, real: String?, type: String = "available", inCall: Boolean = false) =
        WaddleClientEvent.Presence(
            testPresence(
                from = "$ROOM/$nick",
                presenceType = type,
                mucRole = if (type == "unavailable") WaddleMucRole.NONE else WaddleMucRole.PARTICIPANT,
                mucJid = real,
                muji = WaddleMujiPresence(preparing = false, active = true, audio = true, video = false)
                    .takeIf { inCall },
            ),
        )

    private fun roomMessage(
        stanzaId: String,
        nick: String = "alice",
        mentions: List<String> = emptyList(),
        broadcast: String? = null,
        thread: String? = null,
    ) =
        WaddleClientEvent.Message(
            testMessage(
                id = "id-$stanzaId",
                stanzaId = stanzaId,
                stanzaIdBy = ROOM,
                from = "$ROOM/$nick",
                to = null,
                messageType = "groupchat",
                isMuc = true,
                mentionUris = mentions,
                broadcastMention = broadcast,
                thread = thread,
            ),
        )

    @Test
    fun `a reconnect forgets everyone the last stream reported`() = runTest {
        val harness = Harness(this)
        harness.loginReady()
        harness.emit(occupant("bob", "bob@waddle.test/web"))
        harness.emit(WaddleClientEvent.Presence(testPresence(from = "cat@waddle.test/phone", show = "away")))
        assertEquals(PresenceShow.AWAY, harness.manager.presenceStore.contacts.value["cat@waddle.test"]?.show)

        // Bob left and Cat went offline while we were disconnected; only
        // Dee is re-announced on the new stream.
        harness.reconnect()
        harness.emit(occupant("dee", "dee@waddle.test/web"))

        assertTrue(harness.manager.presenceStore.contacts.value.isEmpty())
        assertEquals(setOf("dee"), harness.manager.presenceStore.occupants.value[ROOM]?.keys)
        harness.manager.logout()
    }

    @Test
    fun `mentions of the account count with unread and clear with it`() = runTest {
        val harness = Harness(this)
        harness.loginReady()
        harness.emit(roomMessage("s1", mentions = listOf("xmpp:Icepuma@waddle.test")))
        harness.emit(roomMessage("s2", broadcast = MENTION_URI_EVERYONE))
        harness.emit(roomMessage("s3", mentions = listOf("xmpp:bob@waddle.test")))
        harness.emit(roomMessage("s4", nick = "icepuma", mentions = listOf("xmpp:icepuma@waddle.test")))
        harness.emit(roomMessage("s5", mentions = listOf("xmpp:icepuma@waddle.test"), thread = "root-1"))
        assertEquals(3, harness.manager.unreadStore.counts.value[ROOM])
        assertEquals(mapOf(ROOM to 2), harness.manager.mentionStore.counts.value)

        // A sibling device displayed everything (XEP-0490).
        harness.emit(
            WaddleClientEvent.Message(
                testMessage(
                    id = "mds-1",
                    body = null,
                    mdsDisplayed = listOf(WaddleMdsDisplayedEntry(chatId = ROOM, stanzaId = "s3", stanzaIdBy = ROOM)),
                ),
            ),
        )
        assertTrue(harness.manager.mentionStore.counts.value.isEmpty())

        harness.emit(roomMessage("s6", mentions = listOf("xmpp:icepuma@waddle.test")))
        assertEquals(1, harness.manager.mentionStore.counts.value[ROOM])
        harness.manager.unreadStore.clear(ROOM)
        assertTrue(harness.manager.mentionStore.counts.value.isEmpty())
        harness.manager.logout()
    }

    @Test
    fun `mention counts are live-only and reset on reconnect`() = runTest {
        val harness = Harness(this)
        harness.loginReady()
        harness.emit(roomMessage("s1", broadcast = MENTION_URI_HERE))
        assertEquals(1, harness.manager.mentionStore.counts.value[ROOM])

        harness.reconnect()

        assertTrue(harness.manager.mentionStore.counts.value.isEmpty())
        harness.manager.logout()
    }

    @Test
    fun `the people directory follows presence and huddles, sampled`() = runTest {
        val harness = Harness(this)
        harness.loginReady()
        var latest = PeopleDirectorySnapshot.EMPTY
        val collector = backgroundScope.launch { harness.manager.peopleDirectory.collect { latest = it } }
        runCurrent()

        harness.emit(occupant("icepuma", "icepuma@waddle.test/phone", inCall = true))
        harness.emit(occupant("bob", "bob@waddle.test/web", inCall = true))
        harness.emit(occupant("anon", real = null))
        advanceTimeBy(XmppSessionManager.PEOPLE_DIRECTORY_SAMPLE_MILLIS + 1)
        runCurrent()

        assertEquals(listOf("bob@waddle.test"), latest.people.map { it.bareJid })
        assertEquals(ROOM, latest.people.single().huddleRoomJid)
        assertEquals(RoomHeadcount(here = 2, inHuddle = 2), latest.rooms[ROOM])

        harness.reconnect()
        advanceTimeBy(XmppSessionManager.PEOPLE_DIRECTORY_SAMPLE_MILLIS + 1)
        runCurrent()
        assertTrue("bob left while we were away", latest.people.isEmpty())
        assertTrue(latest.rooms.isEmpty())

        collector.cancel()
        harness.manager.logout()
    }

    private companion object {
        const val ROOM = "room@muc.waddle.test"
        const val RECONNECT_DELAY_MILLIS = 1_001L
    }
}
