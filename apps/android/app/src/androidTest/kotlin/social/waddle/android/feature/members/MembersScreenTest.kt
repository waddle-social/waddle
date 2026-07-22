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
import org.junit.Assert.assertEquals
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
 * Members screen end-to-end through the faked FFI: the §9.5 member
 * list renders the seeded affiliations merged with live presence, and
 * the kick flow (sheet → confirm) records the §8.2 FFI call.
 */
@RunWith(AndroidJUnit4::class)
class MembersScreenTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private lateinit var harness: TestAppGraph

    @Before
    fun setUp() {
        harness = TestAppGraph()
        harness.signInAndConnect()
        harness.activeFakeClient().roomMembersByTier = mapOf(
            WaddleMucAffiliation.OWNER to listOf(
                WaddleRoomMemberEntry(
                    jid = "icepuma@waddle.test",
                    affiliation = WaddleMucAffiliation.OWNER,
                    nick = null,
                    reason = null,
                ),
            ),
            WaddleMucAffiliation.MEMBER to listOf(
                WaddleRoomMemberEntry(
                    jid = "bob@waddle.test",
                    affiliation = WaddleMucAffiliation.MEMBER,
                    nick = null,
                    reason = null,
                ),
            ),
        )
        // Own occupant presence (status 110, owner) unlocks management;
        // bob's presence marks him present with a kickable nick.
        harness.emitPresence(
            testPresence(
                from = "$ROOM/icepuma",
                mucAffiliation = WaddleMucAffiliation.OWNER,
                mucJid = "icepuma@waddle.test/app",
                mucStatusCodes = listOf(110u),
            ),
        )
        harness.emitPresence(
            testPresence(
                from = "$ROOM/bob",
                mucAffiliation = WaddleMucAffiliation.MEMBER,
                mucJid = "bob@waddle.test/phone",
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

    private fun waitForText(text: String) {
        composeRule.waitUntil(timeoutMillis = 10_000) {
            composeRule.onAllNodesWithText(text).fetchSemanticsNodes().isNotEmpty()
        }
    }

    @Test
    fun memberListRendersSeededAffiliationsMergedWithPresence() {
        waitForText("bob@waddle.test")
        composeRule.onNodeWithText("icepuma@waddle.test").assertExists()
        // bob is live: the presence merge shows "Present now".
        waitForText("Present now")
        // Owner + member tier labels render.
        waitForText("Owner")
        waitForText("Member")
    }

    @Test
    fun kickFlowRecordsTheFfiCallAfterConfirmation() {
        waitForText("bob@waddle.test")
        composeRule.onNodeWithTag(MembersScreenTestTags.MEMBER_ROW_PREFIX + "bob").performClick()
        waitForText("Kick")
        composeRule.onNodeWithText("Kick").performClick()
        waitForText("Kick occupant?")
        composeRule.onNodeWithText("Kick").performClick()

        composeRule.waitUntil(timeoutMillis = 10_000) {
            harness.activeFakeClient().kickCalls.isNotEmpty()
        }
        val (roomJid, nick, _) = harness.activeFakeClient().kickCalls.single()
        assertEquals(ROOM, roomJid)
        assertEquals("bob", nick)
    }

    private companion object {
        const val ROOM = "general@muc.waddle.test"
    }
}
