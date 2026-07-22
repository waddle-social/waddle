package social.waddle.android.feature.conversation

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import social.waddle.android.client.GifResult
import social.waddle.android.client.GifSearchGateway
import social.waddle.android.client.GifSearchResult

/** GIF picker sheet: trending grid, selection, and error states. */
@RunWith(AndroidJUnit4::class)
class GifPickerSheetTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private class FakeGateway(private val result: GifSearchResult) : GifSearchGateway {
        override suspend fun search(query: String): GifSearchResult = result
    }

    @Test
    fun selectingAGifEmitsItsOriginalUrl() {
        val gateway = FakeGateway(
            GifSearchResult.Results(
                listOf(
                    GifResult(
                        id = "g1",
                        title = "Penguin dance",
                        previewUrl = "https://media0.giphy.com/g1/200s.gif",
                        originalUrl = "https://media0.giphy.com/g1/giphy.gif",
                    ),
                ),
            ),
        )
        var selected: String? = null
        composeRule.setContent {
            GifPickerSheet(
                gateway = gateway,
                onSelect = { selected = it },
                onDismiss = {},
            )
        }
        composeRule.waitUntil(TIMEOUT_MS) {
            runCatching {
                composeRule.onNodeWithContentDescription("Penguin dance").assertExists()
                true
            }.getOrDefault(false)
        }
        composeRule.onNodeWithContentDescription("Penguin dance").performClick()
        composeRule.runOnIdle {
            assertEquals("https://media0.giphy.com/g1/giphy.gif", selected)
        }
    }

    @Test
    fun notConfiguredStateShowsTheWebCopy() {
        composeRule.setContent {
            GifPickerSheet(
                gateway = FakeGateway(GifSearchResult.NotConfigured),
                onSelect = {},
                onDismiss = {},
            )
        }
        composeRule.waitUntil(TIMEOUT_MS) {
            runCatching {
                composeRule.onNodeWithText("GIF search is not configured.").assertExists()
                true
            }.getOrDefault(false)
        }
        composeRule.onNodeWithText("Powered by GIPHY").assertExists()
    }

    private companion object {
        const val TIMEOUT_MS = 5_000L
    }
}
