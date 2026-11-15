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
import social.waddle.android.feature.people.PeopleScreenTestTags
import social.waddle.android.feature.rooms.RoomsScreenTestTags
import social.waddle.android.feature.sidebar.SidebarTestTags
import social.waddle.android.navigation.MainTab

/**
 * App-state switch and the Navigation 3 shell over a fully-faked graph:
 * SignedOut lands on login; on a phone, Ready opens the Home tab (the
 * conversation list), each tab keeps its own place and back returns
 * Home, and an open page survives recreation.
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

    // The CI emulator is a phone: the tab bar, Home being the conversation list.
    private fun waitForHome() = waitForTag(SidebarTestTags.SCREEN)

    private fun tap(tag: String) {
        composeRule.onNodeWithTag(tag).performClick()
    }

    private fun back() {
        composeRule.runOnUiThread { composeRule.activity.onBackPressedDispatcher.onBackPressed() }
    }

    private fun gone(tag: String) = composeRule.onAllNodesWithTag(tag).fetchSemanticsNodes().isEmpty()

    @Test
    fun signedOutGraphShowsLoginSurface() {
        harness.signOut()
        composeAppShell()
        waitForText("Sign in to your Waddle server")
    }

    @Test
    fun readyGraphOpensOnTheHomeTab() {
        harness.signInAndConnect()
        composeAppShell()
        waitForHome()
        MainTab.entries.forEach { composeRule.onNodeWithTag(it.testTag).assertExists() }
        listOf(SidebarTestTags.BROWSE_ROOMS, SidebarTestTags.ALL_DMS)
            .forEach { composeRule.onNodeWithTag(it).assertExists() }
        // People and You are tabs here, so the list drops them.
        assertTrue(gone(SidebarTestTags.PEOPLE) && gone(SidebarTestTags.YOU) && gone(SidebarTestTags.SETTINGS))
    }

    @Test
    fun everyTabOpensAndBackReturnsHome() {
        harness.signInAndConnect()
        composeAppShell()
        waitForHome()
        tap(MainTab.PEOPLE.testTag)
        waitForTag(PeopleScreenTestTags.SCREEN)
        back()
        waitForHome()
        tap(MainTab.DMS.testTag)
        // From the resource, so a copy change cannot strand the test.
        waitForText(composeRule.activity.getString(R.string.dm_list_empty_title))
        back()
        waitForHome()
        tap(MainTab.YOU.testTag)
        waitForText(composeRule.activity.getString(R.string.you_profile))
        back()
        waitForHome()
        assertFalse("back from a tab must not leave the app", composeRule.activity.isFinishing)
    }

    @Test
    fun eachTabKeepsItsPlace() {
        harness.signInAndConnect()
        composeAppShell()
        waitForHome()
        tap(SidebarTestTags.BROWSE_ROOMS)
        waitForTag(RoomsScreenTestTags.SCREEN)
        // The bar stays over pages; only conversations hide it.
        tap(MainTab.YOU.testTag)
        waitForText(composeRule.activity.getString(R.string.you_profile))
        tap(MainTab.HOME.testTag)
        waitForTag(RoomsScreenTestTags.SCREEN)
        // The shown tab again: back to its root.
        tap(MainTab.HOME.testTag)
        waitForHome()
        composeRule.waitUntil(timeoutMillis = 10_000) { gone(RoomsScreenTestTags.SCREEN) }
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
    fun navigatesYouToSettingsAndBack() {
        harness.signInAndConnect()
        composeAppShell()
        waitForHome()
        tap(MainTab.YOU.testTag)
        composeRule.onNodeWithText(composeRule.activity.getString(R.string.you_settings_body)).performClick()
        waitForText("Appearance")
        composeRule.onNodeWithContentDescription("Back").performClick()
        waitForText(composeRule.activity.getString(R.string.you_profile))
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
        // A pushed page: tab roots have no back arrow.
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
