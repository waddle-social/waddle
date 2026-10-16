package social.waddle.android.push

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
import social.waddle.android.client.FakeClientFactory
import social.waddle.android.client.FakeNetworkSignal
import social.waddle.android.client.InMemoryPreferencesDataStore
import social.waddle.android.client.PinnedRandom
import social.waddle.android.client.ReconnectPolicy
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.push.pushInstanceFor
import social.waddle.android.client.testSessionInfo
import social.waddle.android.service.PushSummary
import social.waddle.client.ffi.WaddleClientEvent

/** When a delivered push becomes a banner, and when the live path owns it. */
@OptIn(ExperimentalCoroutinesApi::class)
class PushMessageHandlerTest {
    private class RecordingSink : PushSummarySink {
        val live = mutableSetOf<String>()
        val posted = mutableListOf<PushSummary>()

        override fun handledLive(item: String): Boolean = item in live

        override suspend fun postPushSummary(summary: PushSummary) {
            posted += summary
        }
    }

    private class Harness(testScope: TestScope, var visible: Boolean = false) {
        val factory = FakeClientFactory()
        val userPrefs = UserPrefs(InMemoryPreferencesDataStore())
        val manager = XmppSessionManager(
            sessionPrefs = SessionPrefs(InMemoryPreferencesDataStore()),
            clientFactory = factory,
            networkSignal = FakeNetworkSignal(),
            userPrefs = userPrefs,
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScope.testScheduler),
        )
        val sink = RecordingSink()
        val handler = PushMessageHandler(
            sessionManager = manager,
            userPrefs = userPrefs,
            notifier = sink,
            isAppVisible = { visible },
            summarize = { envelope ->
                PushSummary(
                    conversationJid = envelope.conversation,
                    isGroupchat = envelope.isGroupchat,
                    isMention = envelope.isMention,
                    item = envelope.item,
                    title = envelope.conversation,
                    text = envelope.pushClass.wire,
                )
            },
        )
    }

    private suspend fun TestScope.signedIn(): Harness {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        return harness
    }

    @Test
    fun `a push while disconnected posts a summary at once`() = runTest {
        val harness = signedIn()

        harness.handler.handle(INSTANCE, DM_PUSH, decrypted = true)

        assertEquals(listOf("alice@waddle.test"), harness.sink.posted.map { it.conversationJid })
        assertEquals("s-1", harness.sink.posted.single().item)
        harness.manager.logout()
    }

    @Test
    fun `untrusted or foreign pushes never post`() = runTest {
        val harness = signedIn()

        harness.handler.handle(INSTANCE, DM_PUSH, decrypted = false)
        harness.handler.handle(pushInstanceFor("someone@else.test"), DM_PUSH, decrypted = true)
        harness.handler.handle(INSTANCE, "garbage".encodeToByteArray(), decrypted = true)

        assertTrue(harness.sink.posted.isEmpty())
        harness.manager.logout()
    }

    @Test
    fun `the open app and disabled notifications post nothing`() = runTest {
        val harness = signedIn()
        harness.visible = true
        harness.handler.handle(INSTANCE, DM_PUSH, decrypted = true)

        harness.visible = false
        harness.userPrefs.setNotificationsEnabled(false)
        harness.handler.handle(INSTANCE, DM_PUSH, decrypted = true)

        assertTrue(harness.sink.posted.isEmpty())
        harness.manager.logout()
    }

    @Test
    fun `a ready stream gets to deliver the message live first`() = runTest {
        val harness = signedIn()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val handling = launch { harness.handler.handle(INSTANCE, DM_PUSH, decrypted = true) }
        runCurrent()
        assertEquals("the stream is probed", 1, harness.factory.clients.last().pingCalls)
        harness.sink.live += "s-1"
        advanceTimeBy(PushMessageHandler.LIVE_GRACE_MILLIS + 1)
        handling.join()

        assertTrue("the live notification already covered it", harness.sink.posted.isEmpty())
        harness.manager.logout()
    }

    @Test
    fun `a ready stream that misses the message still gets the summary`() = runTest {
        val harness = signedIn()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val handling = launch { harness.handler.handle(INSTANCE, DM_PUSH, decrypted = true) }
        advanceTimeBy(PushMessageHandler.LIVE_GRACE_MILLIS + 1)
        handling.join()

        assertEquals(1, harness.sink.posted.size)
        harness.manager.logout()
    }

    private companion object {
        val INSTANCE = pushInstanceFor("icepuma@waddle.test")
        val DM_PUSH = """{"v":1,"class":"dm","conversation":"alice@waddle.test","item":"s-1"}""".encodeToByteArray()
    }
}
