package social.waddle.android.feature.conversation

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.TimelineSource
import social.waddle.android.client.testMessage
import social.waddle.client.ffi.WaddleSafetyCategory
import social.waddle.client.ffi.WaddleSafetyScore
import social.waddle.client.ffi.WaddleSafetyScores

/** XEP-0422 safety-score affordance and breakdown sheet on a MessageCard. */
@RunWith(AndroidJUnit4::class)
class MessageCardSafetyScoresTest {
    private companion object {
        const val HARASSMENT_DESCRIPTION = "Harassment score; view message scores"
    }

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val scores = WaddleSafetyScores(
        modelVersion = "typesafe/jev-1.13-20260917",
        scores = listOf(
            WaddleSafetyScore(WaddleSafetyCategory.IS_QUESTION, 0.92, "is-question-v1"),
            WaddleSafetyScore(WaddleSafetyCategory.HARASSMENT, 0.62, "safety-harassment-v1"),
            WaddleSafetyScore(WaddleSafetyCategory.VIOLENCE, 0.02, "safety-violence-v1"),
        ),
    )

    /** Every safety category under the visibility threshold: no chip, whatever the question signal says. */
    private val quietScores = WaddleSafetyScores(
        modelVersion = "typesafe/jev-1.13-20260917",
        scores = listOf(
            WaddleSafetyScore(WaddleSafetyCategory.IS_QUESTION, 0.92, "is-question-v1"),
            WaddleSafetyScore(WaddleSafetyCategory.HARASSMENT, 0.49, "safety-harassment-v1"),
        ),
    )

    private fun setCard(safetyScores: WaddleSafetyScores?) {
        val message = testMessage(
            from = "room@muc.waddle.test/alice",
            messageType = "groupchat",
            isMuc = true,
            body = "is anyone around?",
        )
        composeRule.setContent {
            MessageCard(
                row = ConversationRow.Stored(
                    TimelineItem(
                        id = "s1",
                        conversationJid = "room@muc.waddle.test",
                        from = message.from,
                        body = message.body.orEmpty(),
                        timestamp = null,
                        isMine = false,
                        source = TimelineSource.Live(message),
                        safetyScores = safetyScores,
                    ),
                ),
                onRetry = {},
            )
        }
    }

    @Test
    fun noAffordanceWithoutScores() {
        setCard(safetyScores = null)
        composeRule.onNodeWithContentDescription(HARASSMENT_DESCRIPTION).assertDoesNotExist()
    }

    @Test
    fun noAffordanceBelowTheNoticeThreshold() {
        setCard(safetyScores = quietScores)
        composeRule.onNodeWithContentDescription(HARASSMENT_DESCRIPTION).assertDoesNotExist()
    }

    @Test
    fun affordanceOpensTheBreakdown() {
        setCard(safetyScores = scores)
        composeRule.onNodeWithContentDescription(HARASSMENT_DESCRIPTION)
            .assertExists()
            .performClick()
        composeRule.onNodeWithText("Message scores").assertExists()
        composeRule.onNodeWithText("Asks a question").assertExists()
        composeRule.onNodeWithText("92%").assertExists()
        // The chip and the breakdown both name this category.
        assertEquals(2, composeRule.onAllNodesWithText("Harassment", useUnmergedTree = true).fetchSemanticsNodes().size)
        composeRule.onNodeWithText("62%").assertExists()
        // Below the visibility threshold: hidden from the breakdown.
        composeRule.onNodeWithText("Violence").assertDoesNotExist()
        composeRule.onNodeWithText("2%").assertDoesNotExist()
        composeRule.onNodeWithText("Model: typesafe/jev-1.13-20260917").assertExists()
    }
}
