package social.waddle.android

import android.Manifest
import androidx.activity.ComponentActivity
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.flow.MutableStateFlow
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import social.waddle.android.navigation.MainTab

/**
 * App-state switch and Navigation 3 shell over a fully-faked graph:
 * SignedOut lands on login, Ready composes Home, and the drawer
 * navigates to Settings / the DM list and back.
 */
@RunWith(AndroidJUnit4::class)
class AppShellNavigationTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private lateinit var harness: TestAppGraph

    @Before
    fun setUp() {
        // The Ready shell fires the POST_NOTIFICATIONS runtime prompt;
        // pre-granting keeps the system dialog from stealing the window
        // focus compose input injection needs.
        InstrumentationRegistry.getInstrumentation().uiAutomation.grantRuntimePermission(
            InstrumentationRegistry.getInstrumentation().targetContext.packageName,
            Manifest.permission.POST_NOTIFICATIONS,
        )
        harness = TestAppGraph()
    }

    @After
    fun tearDown() {
        harness.shutdown()
    }

    private fun composeAppShell() {
        composeRule.setContent {
            CompositionLocalProvider(LocalAppGraph provides harness.graph) {
                AppShell(
                    pendingConversationJid = MutableStateFlow(null),
                    onConversationConsumed = {},
                )
            }
        }
    }

    private fun waitForText(text: String) {
        composeRule.waitUntil(timeoutMillis = 10_000) {
            composeRule.onAllNodesWithText(text).fetchSemanticsNodes().isNotEmpty()
        }
    }

    private fun waitForHome() {
        waitForText("No channels yet. They appear here once the server topology loads.")
    }

    private fun openTab(tab: MainTab) {
        composeRule.onNodeWithTag(tab.testTag).performClick()
    }

    @Test
    fun signedOutGraphShowsLoginSurface() {
        harness.signOut()
        composeAppShell()
        waitForText("Sign in to your Waddle server")
    }

    @Test
    fun readyGraphComposesHomeShell() {
        harness.signInAndConnect()
        composeAppShell()
        waitForHome()
        composeRule.onNodeWithText("Waddle").assertExists()
    }

    @Test
    fun navigatesHomeToSettingsAndBack() {
        harness.signInAndConnect()
        composeAppShell()
        waitForHome()
        openTab(MainTab.YOU)
        waitForText("Settings")
        composeRule.onNodeWithText("Settings").performClick()
        waitForText("Appearance")
        composeRule.onNodeWithContentDescription("Back").performClick()
        // Back lands on the tab Settings was opened from.
        waitForText("Settings")
        openTab(MainTab.HOME)
        waitForHome()
    }

    @Test
    fun navigatesHomeToDmListAndBack() {
        harness.signInAndConnect()
        composeAppShell()
        waitForHome()
        openTab(MainTab.DMS)
        waitForText("No direct messages yet.")
        openTab(MainTab.HOME)
        waitForHome()
    }
}
