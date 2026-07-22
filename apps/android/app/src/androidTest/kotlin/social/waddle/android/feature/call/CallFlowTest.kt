package social.waddle.android.feature.call

import android.Manifest
import android.app.NotificationManager
import androidx.activity.ComponentActivity
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import social.waddle.android.AppShell
import social.waddle.android.LocalAppGraph
import social.waddle.android.TestAppGraph
import social.waddle.android.client.RecordedCallVerb
import social.waddle.android.client.calls.CallState
import social.waddle.android.service.NotificationChannels
import social.waddle.client.ffi.WaddleCallEvent
import social.waddle.client.ffi.WaddleCallEventKind
import social.waddle.client.ffi.WaddleCallMedia
import social.waddle.client.ffi.WaddleLiveKitJoin

/**
 * DM call flows over the fully-faked graph: an inbound XEP-0353
 * propose raises the ring notification + full-screen UI and decline
 * sends `<reject/>`; an outgoing call renders the in-call screen and
 * turns active against the FAKE media controller (no WebRTC, no mic —
 * hermetic by construction).
 */
@RunWith(AndroidJUnit4::class)
class CallFlowTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private lateinit var harness: TestAppGraph

    private val peerFull = "bob@waddle.test/phone"

    @Before
    fun setUp() {
        InstrumentationRegistry.getInstrumentation().uiAutomation.grantRuntimePermission(
            InstrumentationRegistry.getInstrumentation().targetContext.packageName,
            Manifest.permission.POST_NOTIFICATIONS,
        )
        harness = TestAppGraph()
        // The production Application starts these on its own graph; the
        // test graph wires its collectors explicitly.
        harness.graph.callNotifier.start(harness.graph.applicationScope)
        harness.graph.callSessionController.start()
    }

    @After
    fun tearDown() {
        harness.shutdown()
    }

    private fun composeAppShell() {
        composeRule.setContent {
            CompositionLocalProvider(LocalAppGraph provides harness.graph) {
                AppShell(
                    pendingConversationJid = MutableStateFlow(null),
                    onConversationConsumed = {},
                )
            }
        }
    }

    private fun waitForTag(tag: String) {
        composeRule.waitUntil(timeoutMillis = 10_000) {
            composeRule.onAllNodesWithTag(tag).fetchSemanticsNodes().isNotEmpty()
        }
    }

    private fun incomingProposeEvent(sid: String = "c-in-1") = WaddleCallEvent(
        from = peerFull,
        to = null,
        sid = sid,
        kind = WaddleCallEventKind.Propose(WaddleCallMedia(audio = true, video = false)),
    )

    private fun notificationManager(): NotificationManager =
        InstrumentationRegistry.getInstrumentation().targetContext
            .getSystemService(NotificationManager::class.java)

    private fun hasIncomingCallNotification(): Boolean =
        notificationManager().activeNotifications.any { posted ->
            posted.notification.channelId == NotificationChannels.INCOMING_CALLS
        }

    @Test
    fun incomingProposeRaisesRingNotificationAndDeclineSendsReject() {
        harness.signInAndConnect()
        composeAppShell()

        harness.emitCallEvent(incomingProposeEvent())

        // Full-screen UI + the CallStyle ring notification both come up.
        waitForTag(CallTestTags.INCOMING_SCREEN)
        composeRule.waitUntil(timeoutMillis = 10_000) { hasIncomingCallNotification() }

        // XEP-0353 §3.2: the responder rings back the caller's bare JID.
        composeRule.waitUntil(timeoutMillis = 10_000) {
            harness.activeFakeClient().callVerbs
                .any { it == RecordedCallVerb.Ringing("bob@waddle.test", "c-in-1") }
        }

        composeRule.onNodeWithTag(CallTestTags.DECLINE_BUTTON).performClick()

        // Reject to the proposer's FULL JID; slot idle; ring retired.
        composeRule.waitUntil(timeoutMillis = 10_000) {
            harness.activeFakeClient().callVerbs
                .any { it == RecordedCallVerb.Reject(peerFull, "c-in-1") }
        }
        composeRule.waitUntil(timeoutMillis = 10_000) {
            harness.graph.sessionManager.callStore.state.value == CallState.Idle
        }
        composeRule.waitUntil(timeoutMillis = 10_000) { !hasIncomingCallNotification() }
        composeRule.waitUntil(timeoutMillis = 10_000) {
            composeRule.onAllNodesWithTag(CallTestTags.INCOMING_SCREEN)
                .fetchSemanticsNodes().isEmpty()
        }
    }

    @Test
    fun outgoingCallRendersInCallScreenAndConnectsFakeMediaOnAccept() {
        harness.signInAndConnect()
        composeAppShell()

        runBlocking {
            harness.graph.sessionManager.callStore.startCall(
                peerJid = "bob@waddle.test",
                media = WaddleCallMedia(audio = true, video = false),
            )
        }

        // Outgoing phase renders the in-call surface with hang-up.
        waitForTag(CallTestTags.ACTIVE_SCREEN)
        composeRule.onNodeWithTag(CallTestTags.HANG_UP_BUTTON).assertIsDisplayed()
        val sid = (harness.graph.sessionManager.callStore.state.value as CallState.Outgoing).sid

        // Peer answers: proceed → our session-initiate → server-rewritten
        // session-accept with the LiveKit join.
        harness.emitCallEvent(
            WaddleCallEvent(from = peerFull, to = null, sid = sid, kind = WaddleCallEventKind.Proceed),
        )
        composeRule.waitUntil(timeoutMillis = 10_000) {
            harness.activeFakeClient().callVerbs.any { it is RecordedCallVerb.SessionInitiate }
        }
        harness.emitCallEvent(
            WaddleCallEvent(
                from = peerFull,
                to = null,
                sid = sid,
                kind = WaddleCallEventKind.SessionAccept(
                    join = WaddleLiveKitJoin(
                        url = "wss://livekit.waddle.test",
                        room = "dm-room",
                        identity = "icepuma@waddle.test/waddle-android-1",
                        token = "jwt",
                    ),
                    media = WaddleCallMedia(audio = true, video = false),
                ),
            ),
        )

        // The app-scoped controller connected the FAKE media plane.
        composeRule.waitUntil(timeoutMillis = 10_000) { harness.callMedia.connectCalls.isNotEmpty() }
        assertEquals("dm-room", harness.callMedia.connectCalls.single().join.room)

        composeRule.onNodeWithTag(CallTestTags.HANG_UP_BUTTON).performClick()

        composeRule.waitUntil(timeoutMillis = 10_000) {
            harness.activeFakeClient().callVerbs
                .any { it is RecordedCallVerb.SessionTerminateWithOutcome }
        }
        composeRule.waitUntil(timeoutMillis = 10_000) {
            harness.activeFakeClient().callVerbs.any { it is RecordedCallVerb.Finish }
        }
        composeRule.waitUntil(timeoutMillis = 10_000) { harness.callMedia.disconnectCalls > 0 }
        assertTrue(harness.graph.sessionManager.callStore.state.value == CallState.Idle)
    }
}
