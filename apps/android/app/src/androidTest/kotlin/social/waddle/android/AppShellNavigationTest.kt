package social.waddle.android

import android.Manifest
import androidx.activity.ComponentActivity
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.junit4.StateRestorationTester
import androidx.compose.ui.test.junit4.v2.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
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
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import social.waddle.android.feature.home.HomeScreenTestTags
import social.waddle.android.feature.people.PeopleScreenTestTags
import social.waddle.android.feature.rooms.RoomsScreenTestTags
import social.waddle.android.navigation.MainTab

/**
 * App-state switch and the Navigation 3 shell over a fully-faked graph:
 * SignedOut lands on login, Ready composes Home, the five tabs
 * (Home · Rooms · People · DMs · You) each open their screen, back from
 * a tab returns to Home, and the selected tab survives recreation.
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

    private val shell = @androidx.compose.runtime.Composable {
        CompositionLocalProvider(LocalAppGraph provides harness.graph) {
            AppShell(
                pendingConversationJid = MutableStateFlow(null),
                onConversationConsumed = {},
            )
        }
    }

    private fun composeAppShell() {
        composeRule.setContent { shell() }
    }

    private fun waitForText(text: String) {
        composeRule.waitUntil(timeoutMillis = 10_000) {
            composeRule.onAllNodesWithText(text).fetchSemanticsNodes().isNotEmpty()
        }
    }

    private fun waitForTag(tag: String) {
        composeRule.waitUntil(timeoutMillis = 10_000) {
            composeRule.onAllNodesWithTag(tag).fetchSemanticsNodes().isNotEmpty()
        }
    }

    private fun waitForHome() = waitForTag(HomeScreenTestTags.SCREEN)

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
        // The wordmark reads as the app's name.
        composeRule.onNodeWithContentDescription("Waddle").assertExists()
        MainTab.entries.forEach { composeRule.onNodeWithTag(it.testTag).assertExists() }
    }

    @Test
    fun everyTabOpensItsScreen() {
        harness.signInAndConnect()
        composeAppShell()
        waitForHome()
        openTab(MainTab.ROOMS)
        waitForTag(RoomsScreenTestTags.SCREEN)
        openTab(MainTab.PEOPLE)
        waitForTag(PeopleScreenTestTags.SCREEN)
        openTab(MainTab.DMS)
        // From the resource, so a copy change cannot strand the test.
        waitForText(composeRule.activity.getString(R.string.dm_list_empty_title))
        openTab(MainTab.YOU)
        waitForText("Settings")
        openTab(MainTab.HOME)
        waitForHome()
    }

    @Test
    fun emptyCommunityInvitesInsteadOfShowingZero() {
        harness.signInAndConnect()
        composeAppShell()
        waitForHome()
        openTab(MainTab.ROOMS)
        // The fake topology is empty: once it settles, an invitation.
        waitForText("No rooms yet. Create the first one and invite people in.")
    }

    @Test
    fun backFromATabReturnsHome() {
        harness.signInAndConnect()
        composeAppShell()
        waitForHome()
        openTab(MainTab.PEOPLE)
        waitForTag(PeopleScreenTestTags.SCREEN)
        composeRule.runOnUiThread { composeRule.activity.onBackPressedDispatcher.onBackPressed() }
        waitForHome()
        assertFalse("back from a tab must not leave the app", composeRule.activity.isFinishing)
    }

    @Test
    fun navigatesHomeToSettingsAndBack() {
        harness.signInAndConnect()
        composeAppShell()
        waitForHome()
        // The account avatar in the Home header leads to You.
        composeRule.onNodeWithTag(HomeScreenTestTags.ACCOUNT_ACTION).performClick()
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
    fun selectedTabSurvivesRecreation() {
        harness.signInAndConnect()
        val restoration = StateRestorationTester(composeRule)
        restoration.setContent { shell() }
        waitForHome()
        openTab(MainTab.ROOMS)
        waitForTag(RoomsScreenTestTags.SCREEN)
        restoration.emulateSavedInstanceStateRestore()
        waitForTag(RoomsScreenTestTags.SCREEN)
    }

    @Test
    fun tabTopBarsClearTheStatusBar() {
        // The app draws edge to edge; the bare test activity does not by default.
        composeRule.runOnUiThread { composeRule.activity.enableEdgeToEdge() }
        harness.signInAndConnect()
        composeAppShell()
        waitForHome()
        openTab(MainTab.ROOMS)
        waitForTag(RoomsScreenTestTags.SCREEN)
        // The CI emulator image has no system UI, so it reports no status
        // bar: hand the window one, as a phone would.
        composeRule.runOnUiThread {
            val insets = WindowInsetsCompat.Builder()
                .setInsets(WindowInsetsCompat.Type.statusBars(), Insets.of(0, FAKE_STATUS_BAR_PX, 0, 0))
                .build()
            ViewCompat.dispatchApplyWindowInsets(composeRule.activity.window.decorView, insets)
        }
        composeRule.waitForIdle()

        // The tab label says "Rooms" too; the page title is the higher one.
        val titleTop = composeRule.onAllNodesWithText("Rooms")
            .fetchSemanticsNodes()
            .minOf { it.boundsInWindow.top }
        assertTrue(
            "title at $titleTop px sits under the $FAKE_STATUS_BAR_PX px status bar",
            titleTop >= FAKE_STATUS_BAR_PX,
        )
    }

    private companion object {
        const val FAKE_STATUS_BAR_PX = 100
    }
}
