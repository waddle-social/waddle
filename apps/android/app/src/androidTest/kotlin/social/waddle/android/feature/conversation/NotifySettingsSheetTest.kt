package social.waddle.android.feature.conversation

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.assertIsSelected
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.isSelectable
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNode
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import social.waddle.android.TestAppGraph
import social.waddle.android.client.store.ConversationKind
import social.waddle.android.feature.dm.DmViewModel
import social.waddle.client.ffi.WaddleNotifyMode

/**
 * XEP-0492 bell sheet, end-to-end through the faked FFI: picking
 * "Muted" publishes the DM notification mode via the real session
 * manager and reconciles the notify-settings store.
 */
@RunWith(AndroidJUnit4::class)
class NotifySettingsSheetTest {

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
            ConversationScreen(
                title = "alice",
                viewModel = viewModel,
                onBack = {},
                // Non-null: the bell only shows on parent conversations
                // (thread screens share their parent's setting).
                onOpenThread = {},
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

    @Test
    fun mutingThroughTheBellSheetPublishesAndUpdatesTheStore() {
        composeRule.onNodeWithContentDescription("Notifications").performClick()
        waitForText("Muted")
        // Current mode pre-selects: the §3 direct-chat default is
        // "All messages" before any override exists.
        composeRule.onNode(isSelectable() and hasText("All messages")).assertIsSelected()

        composeRule.onNodeWithText("Muted").performClick()

        // The fake client recorded the XEP-0492 set call...
        composeRule.waitUntil(timeoutMillis = 10_000) {
            harness.activeFakeClient().dmNotifyCalls.any { (jid, mode, _) ->
                jid == PEER_JID && mode == WaddleNotifyMode.NEVER
            }
        }
        // ...and the store reconciled from the Ok outcome.
        composeRule.waitUntil(timeoutMillis = 10_000) {
            harness.graph.sessionManager.notifySettingsStore
                .modeFor(PEER_JID, ConversationKind.DIRECT_CHAT) == WaddleNotifyMode.NEVER
        }

        // Reopening pre-selects the stored override.
        composeRule.onNodeWithContentDescription("Notifications").performClick()
        waitForText("Mentions only")
        composeRule.onNode(isSelectable() and hasText("Muted")).assertIsSelected()
    }

    private companion object {
        const val PEER_JID = "alice@waddle.test"
    }
}
