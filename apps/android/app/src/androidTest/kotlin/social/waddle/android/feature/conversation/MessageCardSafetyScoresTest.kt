package social.waddle.android.feature.conversation

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.TimelineSource
import social.waddle.android.client.testMessage
import social.waddle.client.ffi.WaddleSafetyScore
import social.waddle.client.ffi.WaddleSafetyScoreCategory
import social.waddle.client.ffi.WaddleSafetyScoresPayload

/** XEP-0422 safety-score affordance and breakdown sheet on a MessageCard. */
@RunWith(AndroidJUnit4::class)
class MessageCardSafetyScoresTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val scores = WaddleSafetyScoresPayload.Scores(
        modelVersion = "typesafe/jev-1.13-20260917",
        scores = listOf(
            WaddleSafetyScore(WaddleSafetyScoreCategory.IS_QUESTION, 0.92, "is-question-v1"),
            WaddleSafetyScore(WaddleSafetyScoreCategory.HARASSMENT, 0.02, "safety-harassment-v1"),
        ),
    )

    private fun setCard(safetyScores: WaddleSafetyScoresPayload.Scores?) {
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
        composeRule.onNodeWithContentDescription("View automated scores for this message").assertDoesNotExist()
    }

    @Test
    fun affordanceOpensTheBreakdown() {
        setCard(safetyScores = scores)
        composeRule.onNodeWithContentDescription("View automated scores for this message")
            .assertExists()
            .performClick()
        composeRule.onNodeWithText("Message scores").assertExists()
        composeRule.onNodeWithText("Asks a question").assertExists()
        composeRule.onNodeWithText("92%").assertExists()
        composeRule.onNodeWithText("Harassment").assertExists()
        composeRule.onNodeWithText("2%").assertExists()
        composeRule.onNodeWithText("Model: typesafe/jev-1.13-20260917").assertExists()
    }
}
