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
import social.waddle.android.feature.sidebar.SidebarTestTags

/**
 * App-state switch and the Navigation 3 shell over a fully-faked graph:
 * SignedOut lands on login, Ready composes the sidebar, each page it
 * opens pushes over it and back returns, and an open page survives
 * recreation.
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

    private fun waitForHome() = waitForTag(SidebarTestTags.SCREEN)

    private fun tap(tag: String) {
        composeRule.onNodeWithTag(tag).performClick()
    }

    private fun back() {
        composeRule.runOnUiThread { composeRule.activity.onBackPressedDispatcher.onBackPressed() }
    }

    @Test
    fun signedOutGraphShowsLoginSurface() {
        harness.signOut()
        composeAppShell()
        waitForText("Sign in to your Waddle server")
    }

    @Test
    fun readyGraphComposesSidebar() {
        harness.signInAndConnect()
        composeAppShell()
        waitForHome()
        listOf(
            SidebarTestTags.DASHBOARD,
            SidebarTestTags.PEOPLE,
            SidebarTestTags.BROWSE_ROOMS,
            SidebarTestTags.ALL_DMS,
            SidebarTestTags.YOU,
            SidebarTestTags.SETTINGS,
        ).forEach { composeRule.onNodeWithTag(it).assertExists() }
    }

    @Test
    fun everySidebarPageOpensAndBackReturns() {
        harness.signInAndConnect()
        composeAppShell()
        waitForHome()
        tap(SidebarTestTags.DASHBOARD)
        waitForTag(HomeScreenTestTags.SCREEN)
        back()
        waitForHome()
        tap(SidebarTestTags.BROWSE_ROOMS)
        waitForTag(RoomsScreenTestTags.SCREEN)
        back()
        waitForHome()
        tap(SidebarTestTags.PEOPLE)
        waitForTag(PeopleScreenTestTags.SCREEN)
        back()
        waitForHome()
        tap(SidebarTestTags.ALL_DMS)
        // From the resource, so a copy change cannot strand the test.
        waitForText(composeRule.activity.getString(R.string.dm_list_empty_title))
        back()
        waitForHome()
        tap(SidebarTestTags.YOU)
        waitForText(composeRule.activity.getString(R.string.you_profile))
        // The on-screen arrow pops too.
        composeRule.onNodeWithContentDescription("Back").performClick()
        waitForHome()
        assertFalse("back from a page must not leave the app", composeRule.activity.isFinishing)
    }

    @Test
    fun emptyCommunityInvitesInsteadOfShowingZero() {
        harness.signInAndConnect()
        composeAppShell()
        waitForHome()
        tap(SidebarTestTags.BROWSE_ROOMS)
        // The fake topology is empty: once it settles, an invitation.
        waitForText("No rooms yet. Create the first one and invite people in.")
    }

    @Test
    fun dashboardLinksPushOverTheDashboard() {
        harness.signInAndConnect()
        composeAppShell()
        waitForHome()
        tap(SidebarTestTags.DASHBOARD)
        waitForTag(HomeScreenTestTags.SCREEN)
        // The account avatar in the dashboard header leads to You.
        tap(HomeScreenTestTags.ACCOUNT_ACTION)
        waitForText(composeRule.activity.getString(R.string.you_profile))
        back()
        // Back lands on the dashboard, not the sidebar.
        waitForTag(HomeScreenTestTags.SCREEN)
    }

    @Test
    fun navigatesSidebarToSettingsAndBack() {
        harness.signInAndConnect()
        composeAppShell()
        waitForHome()
        tap(SidebarTestTags.SETTINGS)
        waitForText("Appearance")
        composeRule.onNodeWithContentDescription("Back").performClick()
        waitForHome()
    }

    @Test
    fun openPageSurvivesRecreation() {
        harness.signInAndConnect()
        val restoration = StateRestorationTester(composeRule)
        restoration.setContent { shell() }
        waitForHome()
        tap(SidebarTestTags.BROWSE_ROOMS)
        waitForTag(RoomsScreenTestTags.SCREEN)
        restoration.emulateSavedInstanceStateRestore()
        waitForTag(RoomsScreenTestTags.SCREEN)
    }

    @Test
    fun pageTopBarsClearTheStatusBar() {
        // The app draws edge to edge; the bare test activity does not by default.
        composeRule.runOnUiThread { composeRule.activity.enableEdgeToEdge() }
        harness.signInAndConnect()
        composeAppShell()
        waitForHome()
        tap(SidebarTestTags.BROWSE_ROOMS)
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

        val backTop = composeRule.onNodeWithContentDescription("Back")
            .fetchSemanticsNode()
            .boundsInWindow.top
        assertTrue(
            "back arrow at $backTop px sits under the $FAKE_STATUS_BAR_PX px status bar",
            backTop >= FAKE_STATUS_BAR_PX,
        )
    }

    private companion object {
        const val FAKE_STATUS_BAR_PX = 100
    }
}
