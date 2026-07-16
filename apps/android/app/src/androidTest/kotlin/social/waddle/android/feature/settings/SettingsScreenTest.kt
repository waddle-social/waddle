package social.waddle.android.feature.settings

import androidx.activity.ComponentActivity
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsOff
import androidx.compose.ui.test.assertIsOn
import androidx.compose.ui.test.assertIsSelected
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.test.ext.junit.runners.AndroidJUnit4
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import social.waddle.android.LocalAppGraph
import social.waddle.android.TestAppGraph
import social.waddle.android.client.prefs.ThemeMode

/**
 * Settings over the faked graph: the account row reflects the seeded
 * session and every pref control round-trips through the in-memory
 * user DataStore.
 */
@RunWith(AndroidJUnit4::class)
class SettingsScreenTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private lateinit var harness: TestAppGraph

    @Before
    fun setUp() {
        harness = TestAppGraph()
        harness.signInAndConnect()
        composeRule.setContent {
            CompositionLocalProvider(LocalAppGraph provides harness.graph) {
                SettingsScreen(onBack = {})
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

    private fun <T> waitForPref(pref: Flow<T>, expected: T) {
        composeRule.waitUntil(timeoutMillis = 10_000) {
            runBlocking { pref.first() } == expected
        }
    }

    @Test
    fun accountSectionShowsTheSeededSession() {
        waitForText("icepuma")
        composeRule.onNodeWithText("icepuma@waddle.test").assertIsDisplayed()
    }

    @Test
    fun themeSelectionPersistsToUserPrefs() {
        waitForText("Appearance")
        composeRule.onNodeWithText("Dark").performClick()
        waitForPref(harness.graph.userPrefs.theme, ThemeMode.DARK)
        composeRule.onNodeWithText("Dark").assertIsSelected()
        composeRule.onNodeWithText("Light").performClick()
        waitForPref(harness.graph.userPrefs.theme, ThemeMode.LIGHT)
        composeRule.onNodeWithText("Light").assertIsSelected()
    }

    @Test
    fun notificationTogglesRoundTripThroughUserPrefs() {
        waitForText("Show message notifications")
        composeRule.onNodeWithTag(SettingsScreenTestTags.NOTIFICATIONS_TOGGLE).assertIsOn()
        composeRule.onNodeWithTag(SettingsScreenTestTags.NOTIFICATIONS_TOGGLE).performClick()
        waitForPref(harness.graph.userPrefs.notificationsEnabled, false)
        composeRule.onNodeWithTag(SettingsScreenTestTags.NOTIFICATIONS_TOGGLE).assertIsOff()
        composeRule.onNodeWithTag(SettingsScreenTestTags.NOTIFICATIONS_TOGGLE).performClick()
        waitForPref(harness.graph.userPrefs.notificationsEnabled, true)
        composeRule.onNodeWithTag(SettingsScreenTestTags.NOTIFICATIONS_TOGGLE).assertIsOn()
    }

    @Test
    fun soundAndReadReceiptTogglesRoundTripThroughUserPrefs() {
        waitForText("Message sounds")
        composeRule.onNodeWithTag(SettingsScreenTestTags.MESSAGE_SOUNDS_TOGGLE)
            .performScrollTo()
            .performClick()
        waitForPref(harness.graph.userPrefs.messageSoundsEnabled, false)
        composeRule.onNodeWithTag(SettingsScreenTestTags.MESSAGE_SOUNDS_TOGGLE).assertIsOff()
        composeRule.onNodeWithTag(SettingsScreenTestTags.READ_RECEIPTS_TOGGLE)
            .performScrollTo()
            .performClick()
        waitForPref(harness.graph.userPrefs.readReceiptsEnabled, false)
        composeRule.onNodeWithTag(SettingsScreenTestTags.READ_RECEIPTS_TOGGLE).assertIsOff()
        composeRule.onNodeWithTag(SettingsScreenTestTags.READ_RECEIPTS_TOGGLE).performClick()
        waitForPref(harness.graph.userPrefs.readReceiptsEnabled, true)
        composeRule.onNodeWithTag(SettingsScreenTestTags.READ_RECEIPTS_TOGGLE).assertIsOn()
    }
}
