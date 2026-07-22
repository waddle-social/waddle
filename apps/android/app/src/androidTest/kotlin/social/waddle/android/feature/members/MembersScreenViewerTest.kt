package social.waddle.android.feature.members

import androidx.activity.ComponentActivity
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.After
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import social.waddle.android.LocalAppGraph
import social.waddle.android.TestAppGraph
import social.waddle.android.client.testPresence
import social.waddle.client.ffi.WaddleMucAffiliation
import social.waddle.client.ffi.WaddleRoomMemberEntry

/**
 * Negative gating: a plain member (self-presence affiliation MEMBER)
 * gets a read-only members screen — no add-member search, and row
 * taps never open the management sheet. The server would refuse the
 * mutations anyway; this pins the UI-side gate.
 */
@RunWith(AndroidJUnit4::class)
class MembersScreenViewerTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private lateinit var harness: TestAppGraph

    @Before
    fun setUp() {
        harness = TestAppGraph()
        harness.signInAndConnect()
        harness.activeFakeClient().roomMembersByTier = mapOf(
            WaddleMucAffiliation.MEMBER to listOf(
                WaddleRoomMemberEntry(
                    jid = "bob@waddle.test",
                    affiliation = WaddleMucAffiliation.MEMBER,
                    nick = null,
                    reason = null,
                ),
            ),
        )
        // Own presence: plain member — no management privileges.
        harness.emitPresence(
            testPresence(
                from = "$ROOM/icepuma",
                mucAffiliation = WaddleMucAffiliation.MEMBER,
                mucJid = "icepuma@waddle.test/app",
                mucStatusCodes = listOf(110u),
            ),
        )

        composeRule.setContent {
            CompositionLocalProvider(LocalAppGraph provides harness.graph) {
                MembersScreen(roomJid = ROOM, name = "general", onBack = {})
            }
        }
    }

    @After
    fun tearDown() {
        harness.shutdown()
    }

    @Test
    fun nonManagersSeeAReadOnlyList() {
        composeRule.waitUntil(timeoutMillis = 10_000) {
            composeRule.onAllNodesWithText("bob@waddle.test").fetchSemanticsNodes().isNotEmpty()
        }
        // No add-member search for non-managers.
        composeRule.onNodeWithTag(MembersScreenTestTags.SEARCH_FIELD).assertDoesNotExist()

        // Tapping a row must not open the management sheet.
        composeRule.onNodeWithTag(MembersScreenTestTags.MEMBER_ROW_PREFIX + "bob").performClick()
        composeRule.onNodeWithText("Make admin").assertDoesNotExist()
        composeRule.onNodeWithText("Ban").assertDoesNotExist()
        composeRule.onNodeWithText("Kick").assertDoesNotExist()

        // And nothing reached the FFI.
        assertTrue(harness.activeFakeClient().setAffiliationCalls.isEmpty())
        assertTrue(harness.activeFakeClient().kickCalls.isEmpty())
    }

    private companion object {
        const val ROOM = "general@muc.waddle.test"
    }
}
