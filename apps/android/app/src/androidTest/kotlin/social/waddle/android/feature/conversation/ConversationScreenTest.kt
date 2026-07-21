package social.waddle.android.feature.conversation

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasSetTextAction
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.longClick
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.performTouchInput
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import social.waddle.android.TestAppGraph
import social.waddle.android.client.testMessage
import social.waddle.android.feature.dm.DmViewModel

/**
 * DM conversation over the shared scaffold, driven end-to-end through
 * the faked FFI: live messages land via `factory.emit(...)`, sends go
 * through the real session manager into the fake client.
 */
@RunWith(AndroidJUnit4::class)
class ConversationScreenTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private lateinit var harness: TestAppGraph
    private lateinit var viewModel: DmViewModel

    @Before
    fun setUp() {
        harness = TestAppGraph()
        harness.signInAndConnect()
        viewModel = DmViewModel(
            sessionRuntime = harness.graph.sessionRuntime,
            peerJid = PEER_JID,
        )
        composeRule.setContent {
            ConversationScreen(
                title = "alice",
                viewModel = viewModel,
                onBack = {},
                onOpenThread = null,
            )
        }
    }

    @After
    fun tearDown() {
        harness.shutdown()
    }

    private fun waitForText(text: String) {
        composeRule.waitUntil(timeoutMillis = 10_000) {
            composeRule.onAllNodesWithText(text).fetchSemanticsNodes().isNotEmpty()
        }
    }

    private fun emitPeerMessage(id: String, body: String) {
        harness.emitLiveMessage(
            testMessage(
                id = id,
                from = PEER_JID,
                to = "icepuma@waddle.test",
                body = body,
            ),
        )
    }

    @Test
    fun liveMessagesRenderInTheTimeline() {
        emitPeerMessage(id = "live-1", body = "first live message")
        emitPeerMessage(id = "live-2", body = "second live message")
        waitForText("first live message")
        waitForText("second live message")
        composeRule.onAllNodesWithText("alice")
            .fetchSemanticsNodes()
            .let { nodes -> assertTrue("author name renders", nodes.isNotEmpty()) }
    }

    @Test
    fun sendShowsTheSynthesizedOwnDmEcho() {
        composeRule.onNode(hasSetTextAction()).performTextInput("hi there penguin")
        composeRule.onNodeWithContentDescription("Send").performClick()
        composeRule.waitUntil(timeoutMillis = 10_000) {
            val fake = harness.activeFakeClient()
            fake.sendCalls.count { it == PEER_JID to "hi there penguin" } == 1 &&
                fake.sendOptions.size == 1
        }
        val options = checkNotNull(harness.activeFakeClient().sendOptions.single())
        val clientStanzaId = options.stanzaId
        composeRule.waitUntil(timeoutMillis = 10_000) {
            val timeline = harness.graph.sessionRuntime.timelineStore.timeline(PEER_JID).value
            timeline.size == 1 &&
                timeline.single().body == "hi there penguin" &&
                clientStanzaId in timeline.single().identityIds
        }
        val timeline = harness.graph.sessionRuntime.timelineStore.timeline(PEER_JID).value
        assertEquals(1, timeline.size)
        assertEquals("hi there penguin", timeline.single().body)
        assertTrue(clientStanzaId in timeline.single().identityIds)
        waitForText("hi there penguin")
        assertTrue(composeRule.onAllNodesWithText("Sending…").fetchSemanticsNodes().isEmpty())
    }

    @Test
    fun replyModeShowsComposerBannerAndCancels() {
        emitPeerMessage(id = "reply-target", body = "reply to me")
        waitForText("reply to me")
        // The real user path: long-press opens the action sheet, Reply
        // arms the composer.
        composeRule.onNodeWithText("reply to me").performTouchInput { longClick() }
        waitForText("Reply")
        composeRule.onNodeWithText("Reply").performClick()
        waitForText("Replying to alice")
        composeRule.onNodeWithContentDescription("Cancel reply").performClick()
        composeRule.waitUntil(timeoutMillis = 10_000) {
            composeRule.onAllNodesWithText("Replying to alice").fetchSemanticsNodes().isEmpty()
        }
        composeRule.onNodeWithText("reply to me").assertIsDisplayed()
    }

    private companion object {
        const val PEER_JID = "alice@waddle.test"
    }
}
