package social.waddle.android.feature.profile

import androidx.activity.ComponentActivity
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextClearance
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
import social.waddle.android.client.RecordedProfileVerb
import social.waddle.android.client.testVcard4

/**
 * Profile screen over the faked graph: the vCard editor loads the
 * seeded profile and a save publishes the edited vCard4 over the FFI.
 */
@RunWith(AndroidJUnit4::class)
class ProfileScreenTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private lateinit var harness: TestAppGraph

    @Before
    fun setUp() {
        harness = TestAppGraph()
        harness.signInAndConnect()
        harness.activeFakeClient().vcard4 = testVcard4(fullName = "Ice Puma", nickname = "ice")
        composeRule.setContent {
            CompositionLocalProvider(LocalAppGraph provides harness.graph) {
                ProfileScreen(onBack = {})
            }
        }
    }

    @After
    fun tearDown() {
        harness.shutdown()
    }

    private fun waitForProfileLoad() {
        composeRule.waitUntil(timeoutMillis = 10_000) {
            harness.activeFakeClient().fetchVcard4Calls.isNotEmpty() &&
                harness.graph.sessionManager.profileStore.selfVcard.value != null
        }
        composeRule.waitForIdle()
    }

    @Test
    fun savingTheVcardPublishesTheEditedProfile() {
        waitForProfileLoad()
        composeRule.onNodeWithTag(ProfileScreenTestTags.VCARD_FULL_NAME)
            .performScrollTo()
            .assertIsDisplayed()
        composeRule.onNodeWithTag(ProfileScreenTestTags.VCARD_FULL_NAME).performTextClearance()
        composeRule.onNodeWithTag(ProfileScreenTestTags.VCARD_FULL_NAME)
            .performTextInput("Emperor Penguin")
        composeRule.onNodeWithTag(ProfileScreenTestTags.VCARD_SAVE)
            .performScrollTo()
            .performClick()

        composeRule.waitUntil(timeoutMillis = 10_000) {
            harness.activeFakeClient().profileVerbs
                .filterIsInstance<RecordedProfileVerb.PublishVcard4>()
                .isNotEmpty()
        }
        val published = harness.activeFakeClient().profileVerbs
            .filterIsInstance<RecordedProfileVerb.PublishVcard4>()
            .single()
        assertEquals("Emperor Penguin", published.vcard.fullName)
        assertEquals("ice", published.vcard.nickname)
    }
}
