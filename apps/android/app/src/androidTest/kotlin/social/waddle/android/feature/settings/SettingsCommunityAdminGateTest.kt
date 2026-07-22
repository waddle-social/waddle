package social.waddle.android.feature.settings

import androidx.activity.ComponentActivity
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithTag
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.After
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import social.waddle.android.LocalAppGraph
import social.waddle.android.TestAppGraph
import social.waddle.client.ffi.WaddleClientEvent

/**
 * The "Community admin" section is gated on the best-effort
 * `is_community_owner` probe: hidden for everyone else. The probe is
 * UX gating only — the server re-authorizes every admin command.
 */
@RunWith(AndroidJUnit4::class)
class SettingsCommunityAdminGateTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private lateinit var harness: TestAppGraph

    @After
    fun tearDown() {
        harness.shutdown()
    }

    private fun renderSettings() {
        composeRule.setContent {
            CompositionLocalProvider(LocalAppGraph provides harness.graph) {
                SettingsScreen(onBack = {}, onOpenCommunityUsers = {})
            }
        }
    }

    @Test
    fun communityAdminSectionHiddenForNonOwners() {
        harness = TestAppGraph()
        harness.signInAndConnect()
        renderSettings()

        // The account row renders (screen is alive)...
        composeRule.waitUntil(timeoutMillis = 10_000) {
            composeRule.onAllNodesWithText("icepuma").fetchSemanticsNodes().isNotEmpty()
        }
        // ...but the probe answered false: no admin entry.
        composeRule.onNodeWithTag(SettingsScreenTestTags.COMMUNITY_USERS_ROW).assertDoesNotExist()
    }

    @Test
    fun communityAdminSectionAppearsForTheOwner() {
        harness = TestAppGraph()
        harness.signIn()
        composeRule.waitUntil(timeoutMillis = 10_000) {
            harness.clientFactory.clients.isNotEmpty()
        }
        // Canned owner answer must be in place before the Ready probe.
        harness.activeFakeClient().directory.communityOwner = true
        harness.clientFactory.emit(WaddleClientEvent.Connected)
        renderSettings()

        composeRule.waitUntil(timeoutMillis = 10_000) {
            composeRule.onAllNodes(hasTestTag(SettingsScreenTestTags.COMMUNITY_USERS_ROW))
                .fetchSemanticsNodes().isNotEmpty()
        }
    }
}
