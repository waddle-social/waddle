package social.waddle.android.feature.conversation

import androidx.activity.ComponentActivity
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextContains
import androidx.compose.ui.test.hasSetTextAction
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Pure composable contract of [MessageComposer]; no app graph. */
@RunWith(AndroidJUnit4::class)
class MessageComposerTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    @Test
    fun typingAndSendInvokesCallbackAndClearsTheDraft() {
        val sent = mutableListOf<String>()
        composeRule.setContent {
            MessageComposer(onSend = { sent += it })
        }
        composeRule.onNodeWithContentDescription("Send").assertIsNotEnabled()
        composeRule.onNode(hasSetTextAction()).performTextInput("hello penguins")
        composeRule.onNodeWithContentDescription("Send").performClick()
        composeRule.runOnIdle {
            assertEquals(listOf("hello penguins"), sent)
        }
        // A cleared draft disables the send button again.
        composeRule.onNodeWithContentDescription("Send").assertIsNotEnabled()
    }

    @Test
    fun editModePrefillsOriginalBodyAndCancelRestoresTheStashedDraft() {
        val editing = mutableStateOf<ComposerMode.Editing?>(null)
        composeRule.setContent {
            MessageComposer(
                onSend = {},
                editing = editing.value,
                onCancelEdit = { editing.value = null },
            )
        }
        composeRule.onNode(hasSetTextAction()).performTextInput("draft in progress")
        composeRule.runOnIdle {
            editing.value = ComposerMode.Editing(targetId = "t1", originalBody = "original body")
        }
        composeRule.onNodeWithText("Editing message").assertIsDisplayed()
        composeRule.onNode(hasSetTextAction()).assertTextContains("original body")
        composeRule.onNodeWithContentDescription("Cancel edit").performClick()
        composeRule.waitUntil(timeoutMillis = 10_000) {
            composeRule.onAllNodesWithText("Editing message").fetchSemanticsNodes().isEmpty()
        }
        composeRule.onNode(hasSetTextAction()).assertTextContains("draft in progress")
    }

    @Test
    fun replyBannerShowsTheAuthorAndCancels() {
        val replying = mutableStateOf<ComposerMode.Replying?>(
            ComposerMode.Replying(
                targetId = "t1",
                authorJid = "alice@waddle.test",
                authorName = "alice",
                previewBody = "the parent message",
                threadId = null,
            ),
        )
        composeRule.setContent {
            MessageComposer(
                onSend = {},
                replying = replying.value,
                onCancelReply = { replying.value = null },
            )
        }
        composeRule.onNodeWithText("Replying to alice").assertIsDisplayed()
        composeRule.onNodeWithContentDescription("Cancel reply").performClick()
        composeRule.waitUntil(timeoutMillis = 10_000) {
            composeRule.onAllNodesWithText("Replying to alice").fetchSemanticsNodes().isEmpty()
        }
    }
}
