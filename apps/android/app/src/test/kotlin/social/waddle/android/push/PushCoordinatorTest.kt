package social.waddle.android.push

import android.app.Application
import androidx.test.core.app.ApplicationProvider
import com.google.crypto.tink.apps.fixed_webpush.WebPushHybridEncrypt
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
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
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import social.waddle.android.client.FakeClientFactory
import social.waddle.android.client.FakeNetworkSignal
import social.waddle.android.client.InMemoryPreferencesDataStore
import social.waddle.android.client.PinnedRandom
import social.waddle.android.client.ReconnectPolicy
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.prefs.PushEndpoint
import social.waddle.android.client.prefs.PushTransportChoice
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.push.pushInstanceFor
import social.waddle.android.client.testSessionInfo
import social.waddle.android.service.PushSummary
import java.util.Base64

/**
 * Which transport holds the account's one endpoint, and the FCM path end
 * to end: token to endpoint, ciphertext to notification, sign-out to a
 * dead token.
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], application = Application::class)
class PushCoordinatorTest {
    private class FakeFcm : FcmTokens {
        var current: String? = "fcm-token-1"
        var deleteSucceeds = true
        var autoInit: Boolean? = null
        val calls = mutableListOf<String>()

        override suspend fun token(): String? {
            calls += "token"
            return current
        }

        override suspend fun delete(): Boolean {
            calls += "delete"
            return deleteSucceeds
        }

        override fun setAutoInit(enabled: Boolean) {
            autoInit = enabled
        }
    }

    private class PlainWrapper : KeyWrapper {
        override fun wrap(plain: ByteArray): String = url.encodeToString(plain)

        override fun unwrap(wrapped: String): ByteArray? = runCatching { urlDecoder.decode(wrapped) }.getOrNull()
    }

    private class RecordingSink : PushSummarySink {
        val posted = mutableListOf<PushSummary>()

        override fun handledLive(item: String): Boolean = false

        override suspend fun postPushSummary(summary: PushSummary, stillWanted: suspend () -> Boolean) {
            if (stillWanted()) posted += summary
        }
    }

    private class Harness(testScope: TestScope, val fcm: FakeFcm?) {
        val userPrefs = UserPrefs(InMemoryPreferencesDataStore())
        val sessionPrefs = SessionPrefs(InMemoryPreferencesDataStore())
        val manager = XmppSessionManager(
            sessionPrefs = sessionPrefs,
            clientFactory = FakeClientFactory(),
            networkSignal = FakeNetworkSignal(),
            userPrefs = userPrefs,
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScope.testScheduler),
        )
        val sink = RecordingSink()
        val coordinator = PushCoordinator(
            scope = testScope.backgroundScope,
            sessionManager = manager,
            userPrefs = userPrefs,
            distributor = PushDistributor(
                context = ApplicationProvider.getApplicationContext(),
                scope = testScope.backgroundScope,
                sessionManager = manager,
                sessionPrefs = sessionPrefs,
                userPrefs = userPrefs,
            ),
            fcm = fcm,
            keyWrapper = PlainWrapper(),
            handler = PushMessageHandler(
                sessionManager = manager,
                userPrefs = userPrefs,
                notifier = sink,
                isAppVisible = { false },
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
            ),
        )

        suspend fun endpoint(): PushEndpoint? = manager.pushState.first()?.endpoint
    }

    private suspend fun TestScope.signedIn(fcm: FakeFcm? = FakeFcm()): Harness {
        val harness = Harness(this, fcm)
        harness.manager.login(testSessionInfo())
        runCurrent()
        return harness
    }

    @Test
    fun `with fcm and no distributor the account registers over fcm`() = runTest {
        val harness = signedIn()

        assertEquals(PushTransport.FCM, harness.coordinator.transport())
        harness.coordinator.onAccountReady()

        val endpoint = harness.endpoint() as PushEndpoint.Fcm
        assertEquals("fcm-token-1", endpoint.token)
        assertEquals(65, urlDecoder.decode(endpoint.p256dh).size)
        assertEquals(true, harness.fcm?.autoInit)
        harness.manager.logout()
    }

    @Test
    fun `a rotated token keeps the keys`() = runTest {
        val harness = signedIn()
        harness.coordinator.onAccountReady()
        val first = harness.endpoint() as PushEndpoint.Fcm

        harness.coordinator.onFcmToken("fcm-token-2")

        val rotated = harness.endpoint() as PushEndpoint.Fcm
        assertEquals("fcm-token-2", rotated.token)
        assertEquals(first.copy(token = "fcm-token-2"), rotated)
        harness.manager.logout()
    }

    @Test
    fun `a message encrypted to the registered keys becomes a notification`() = runTest {
        val harness = signedIn()
        harness.coordinator.onAccountReady()
        val endpoint = harness.endpoint() as PushEndpoint.Fcm

        harness.coordinator.onFcmMessage(mapOf(PushCoordinator.FCM_PAYLOAD_KEY to encryptTo(endpoint, DM_ENVELOPE)))

        assertEquals("alice@waddle.test", harness.sink.posted.single().conversationJid)
        harness.manager.logout()
    }

    @Test
    fun `messages this account cannot decrypt are dropped`() = runTest {
        val harness = signedIn()
        val stranger = FcmEnvelopeKeys.generate(PlainWrapper()).let {
            PushEndpoint.Fcm("other", it.p256dh, it.auth, it.wrappedPrivateKey)
        }

        // No endpoint yet: nothing to decrypt with.
        harness.coordinator.onFcmMessage(mapOf(PushCoordinator.FCM_PAYLOAD_KEY to encryptTo(stranger, DM_ENVELOPE)))
        harness.coordinator.onAccountReady()
        // Encrypted to someone else's keys, garbage, and no payload at all.
        harness.coordinator.onFcmMessage(mapOf(PushCoordinator.FCM_PAYLOAD_KEY to encryptTo(stranger, DM_ENVELOPE)))
        harness.coordinator.onFcmMessage(mapOf(PushCoordinator.FCM_PAYLOAD_KEY to "garbage"))
        harness.coordinator.onFcmMessage(emptyMap())

        assertTrue(harness.sink.posted.isEmpty())
        harness.manager.logout()
    }

    @Test
    fun `a token that could not be deleted at sign-out dies before the next is fetched`() = runTest {
        val harness = signedIn()
        val fcm = checkNotNull(harness.fcm)
        harness.coordinator.onAccountReady()
        fcm.deleteSucceeds = false

        harness.coordinator.unregister()

        assertEquals(false, fcm.autoInit)
        assertTrue(harness.userPrefs.fcmTokenDeletionPending.first())

        // Offline again on the next registration: no token is fetched.
        fcm.calls.clear()
        harness.coordinator.onAccountReady()
        assertEquals(listOf("delete"), fcm.calls)

        fcm.deleteSucceeds = true
        fcm.calls.clear()
        harness.coordinator.onAccountReady()
        assertEquals(listOf("delete", "token"), fcm.calls)
        assertFalse(harness.userPrefs.fcmTokenDeletionPending.first())
        harness.manager.logout()
    }

    @Test
    fun `a unifiedpush choice without an installed distributor still pushes over fcm`() = runTest {
        val harness = signedIn()
        harness.userPrefs.setPushTransport(PushTransportChoice.UNIFIED_PUSH)

        assertEquals(PushTransport.FCM, harness.coordinator.transport())
        harness.manager.logout()
    }

    @Test
    fun `without fcm unifiedpush is the transport and fcm callbacks do nothing`() = runTest {
        val harness = signedIn(fcm = null)

        assertEquals(PushTransport.UNIFIED_PUSH, harness.coordinator.transport())
        assertFalse(harness.coordinator.fcmAvailable)
        harness.coordinator.onAccountReady()
        harness.coordinator.onFcmToken("fcm-token-x")

        assertNull(harness.endpoint())
        harness.manager.logout()
    }

    @Test
    fun `a late unifiedpush endpoint does not displace the fcm endpoint`() = runTest {
        val harness = signedIn()
        harness.coordinator.onAccountReady()

        harness.coordinator.onUnifiedPushEndpoint(INSTANCE, "https://push.example/up/1", "p256dh", "auth")
        harness.coordinator.onUnifiedPushUnregistered(INSTANCE)

        assertNotNull(harness.endpoint() as? PushEndpoint.Fcm)
        harness.manager.logout()
    }

    private fun encryptTo(endpoint: PushEndpoint.Fcm, plaintext: ByteArray): String =
        url.encodeToString(
            WebPushHybridEncrypt.Builder()
                .withAuthSecret(urlDecoder.decode(endpoint.auth))
                .withRecipientPublicKey(urlDecoder.decode(endpoint.p256dh))
                .build()
                .encrypt(plaintext, null),
        )

    private companion object {
        val url: Base64.Encoder = Base64.getUrlEncoder().withoutPadding()
        val urlDecoder: Base64.Decoder = Base64.getUrlDecoder()
        val INSTANCE = pushInstanceFor("icepuma@waddle.test")
        val DM_ENVELOPE = """{"v":1,"class":"dm","conversation":"alice@waddle.test","item":"s-1"}""".encodeToByteArray()
    }
}
