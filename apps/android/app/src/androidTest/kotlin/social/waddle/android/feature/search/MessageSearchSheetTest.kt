package social.waddle.android.feature.search

import androidx.activity.ComponentActivity
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.hasSetTextAction
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.After
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import social.waddle.android.LocalAppGraph
import social.waddle.android.TestAppGraph
import social.waddle.android.client.testArchivedMessage
import social.waddle.android.client.testMamPage
import social.waddle.android.feature.conversation.ConversationScreen
import social.waddle.android.feature.dm.DmViewModel

/**
 * The full user path through the faked FFI: the top-bar Search action
 * opens the sheet, typing runs a debounced archive query against the
 * fake client, and the returned match renders as a result row.
 */
@RunWith(AndroidJUnit4::class)
class MessageSearchSheetTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private lateinit var harness: TestAppGraph
    private lateinit var viewModel: DmViewModel

    @Before
    fun setUp() {
        harness = TestAppGraph()
        harness.signInAndConnect()
        viewModel = DmViewModel(
            sessionManager = harness.graph.sessionManager,
            peerJid = PEER_JID,
        )
        composeRule.setContent {
            CompositionLocalProvider(LocalAppGraph provides harness.graph) {
                ConversationScreen(
                    title = "alice",
                    viewModel = viewModel,
                    onBack = {},
                    onOpenThread = null,
                    searchTarget = MessageSearchTarget(PEER_JID, isGroupchat = false),
                )
            }
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

    @Test
    fun searchFromTheTopBarRendersAMatchingArchiveRow() {
        // Arm the archive only after the screen's initial history fetch
        // (which shares the fake's canned page) has drained — the match
        // below must be reachable through the search verb alone.
        composeRule.waitUntil(timeoutMillis = 10_000) {
            harness.activeFakeClient().fetchHistoryCalls.isNotEmpty()
        }
        harness.activeFakeClient().mamPage = testMamPage(
            messages = listOf(
                testArchivedMessage(
                    mamId = "search-hit-1",
                    body = "archived penguin fact",
                    timestamp = "2026-07-15T10:00:00Z",
                ),
            ),
        )

        composeRule.onNodeWithContentDescription("Search messages").performClick()
        // Structural + label match: the composer is also settable, and
        // matching the label ("Search messages") instead of placeholder
        // copy keeps the test immune to punctuation churn.
        composeRule.waitUntil(timeoutMillis = 10_000) {
            composeRule.onAllNodesWithText("Search messages")
                .fetchSemanticsNodes().isNotEmpty()
        }
        composeRule.onNode(hasSetTextAction() and hasText("Search messages"))
            .performTextInput("penguin")

        waitForText("archived penguin fact")
        // The result stays out of the timeline: it renders in the sheet
        // because the DM search verb answered, not because the store
        // gained a row.
        assertTrue(
            harness.activeFakeClient().searchCalls.any {
                !it.isRoom && it.conversationJid == PEER_JID && it.query == "penguin"
            },
        )
        assertTrue(
            harness.graph.sessionManager.timelineStore.timeline(PEER_JID).value.isEmpty(),
        )
    }

    private companion object {
        const val PEER_JID = "alice@waddle.test"
    }
}
