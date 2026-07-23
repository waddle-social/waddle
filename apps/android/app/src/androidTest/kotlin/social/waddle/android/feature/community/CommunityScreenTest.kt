package social.waddle.android.feature.community

import androidx.activity.ComponentActivity
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import social.waddle.android.LocalAppGraph
import social.waddle.android.TestAppGraph
import social.waddle.android.client.testFeedEntry
import social.waddle.client.ffi.WaddleFeedSourceKind

/**
 * The XEP-0472 community Feed tab end-to-end through the faked FFI:
 * canned feed entries render newest first with their bridge-source
 * badges, and a composer send publishes through the feed verb.
 */
@RunWith(AndroidJUnit4::class)
class CommunityScreenTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private lateinit var harness: TestAppGraph

    @Before
    fun setUp() {
        harness = TestAppGraph()
        harness.clientFactory.onCreate = { client ->
            client.community.feedEntries = listOf(
                testFeedEntry(
                    id = "post-1",
                    body = "Welcome to the community feed",
                    publishedEpochMs = 2_000L,
                ),
                testFeedEntry(
                    id = "post-mood",
                    body = "feeling great",
                    publishedEpochMs = 1_000L,
                    source = WaddleFeedSourceKind.MOOD,
                ),
            )
            client.community.publishedItemId = "post-new"
        }
        harness.signInAndConnect()
    }

    @After
    fun tearDown() {
        harness.shutdown()
    }

    @Test
    fun feedRendersCannedEntriesAndPublishesAComposerPost() {
        composeRule.setContent {
            CompositionLocalProvider(LocalAppGraph provides harness.graph) {
                CommunityScreen(onBack = {})
            }
        }

        // The init refresh lands the canned entries, newest first.
        composeRule.waitUntil(timeoutMillis = 5_000) {
            composeRule.onAllNodesWithTag(CommunityTestTags.ENTRY_PREFIX + "post-1")
                .fetchSemanticsNodes().isNotEmpty()
        }
        composeRule.onNodeWithText("Welcome to the community feed").assertExists()
        // The bridged entry renders its source badge.
        composeRule.onNodeWithText("Mood update").assertExists()

        // The reconciling refresh after publish serves the server's
        // view, which includes the accepted post.
        harness.activeFakeClient().community.feedEntries = listOf(
            testFeedEntry(
                id = "post-new",
                body = "Hello from Android",
                publishedEpochMs = 3_000L,
            ),
        ) + harness.activeFakeClient().community.feedEntries

        composeRule.onNodeWithTag(CommunityTestTags.COMPOSER_FIELD)
            .performTextInput("Hello from Android")
        composeRule.onNodeWithTag(CommunityTestTags.COMPOSER_SEND).performClick()

        composeRule.waitUntil(timeoutMillis = 5_000) {
            harness.activeFakeClient().community.publishFeedCalls.isNotEmpty()
        }
        assertEquals(
            listOf("Hello from Android" to null),
            harness.activeFakeClient().community.publishFeedCalls.toList(),
        )
        // The optimistic prepend (then reconciling refresh) surfaces the post.
        composeRule.waitUntil(timeoutMillis = 5_000) {
            composeRule.onAllNodesWithTag(CommunityTestTags.ENTRY_PREFIX + "post-new")
                .fetchSemanticsNodes().isNotEmpty()
        }
    }
}
