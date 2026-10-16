package social.waddle.android

import android.Manifest
import androidx.activity.ComponentActivity
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.core.graphics.Insets
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.flow.MutableStateFlow
import org.junit.After
import org.junit.Assert.assertTrue
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
    fun tabTopBarsClearTheStatusBar() {
        // The app draws edge to edge; the bare test activity does not by default.
        composeRule.runOnUiThread { composeRule.activity.enableEdgeToEdge() }
        harness.signInAndConnect()
        composeAppShell()
        waitForHome()
        openTab(MainTab.ACTIVITY)
        waitForText("Activity")
        // The CI emulator image has no system UI, so it reports no status
        // bar: hand the window one, as a phone would.
        composeRule.runOnUiThread {
            val insets = WindowInsetsCompat.Builder()
                .setInsets(WindowInsetsCompat.Type.statusBars(), Insets.of(0, FAKE_STATUS_BAR_PX, 0, 0))
                .build()
            ViewCompat.dispatchApplyWindowInsets(composeRule.activity.window.decorView, insets)
        }
        composeRule.waitForIdle()

        // The tab label says "Activity" too; the title is the higher one.
        val titleTop = composeRule.onAllNodesWithText("Activity")
            .fetchSemanticsNodes()
            .minOf { it.boundsInWindow.top }
        assertTrue(
            "title at $titleTop px sits under the $FAKE_STATUS_BAR_PX px status bar",
            titleTop >= FAKE_STATUS_BAR_PX,
        )
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

    private companion object {
        const val FAKE_STATUS_BAR_PX = 100
    }
}
