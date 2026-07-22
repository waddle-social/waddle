package social.waddle.android.feature.dm

import androidx.activity.ComponentActivity
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import social.waddle.android.LocalAppGraph
import social.waddle.android.TestAppGraph
import social.waddle.android.client.testChannel
import social.waddle.android.client.testInboxEntry
import social.waddle.android.client.testInboxResult
import social.waddle.client.ffi.WaddleTopology

/**
 * The merged DM surface end-to-end through the faked FFI: a group DM
 * from the canned bookmark topology + inbox entry renders on the DM
 * list, and tapping it hands the room to the group-DM destination.
 */
@RunWith(AndroidJUnit4::class)
class DmListScreenTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private lateinit var harness: TestAppGraph

    @Before
    fun setUp() {
        harness = TestAppGraph()
        harness.clientFactory.onCreate = { client ->
            client.topology.result = WaddleTopology(
                spaces = emptyList(),
                channels = listOf(
                    testChannel(ROOM, name = "Alice, Bob", isGroupDm = true),
                ),
            )
            client.inbox.inboxResult = testInboxResult(
                conversations = listOf(
                    testInboxEntry(partner = ROOM, kind = "muc", unread = 2u),
                ),
            )
        }
        harness.signInAndConnect()
    }

    @After
    fun tearDown() {
        harness.shutdown()
    }

    @Test
    fun groupDmFromCannedTopologyRendersAndOpens() {
        var opened: Pair<String, String>? = null
        composeRule.setContent {
            CompositionLocalProvider(LocalAppGraph provides harness.graph) {
                DmListScreen(
                    onOpenDm = { _, _ -> },
                    onOpenGroupDm = { roomJid, name -> opened = roomJid to name },
                    onBack = {},
                )
            }
        }

        val rowTag = DmListTestTags.GROUP_ROW_PREFIX + ROOM
        composeRule.waitUntil(timeoutMillis = 5_000) {
            composeRule.onAllNodesWithTag(rowTag).fetchSemanticsNodes().isNotEmpty()
        }
        composeRule.onNodeWithTag(rowTag).performClick()

        composeRule.waitUntil(timeoutMillis = 5_000) { opened != null }
        assertEquals(ROOM to "Alice, Bob", opened)
    }

    @Test
    fun openingTheGroupDmRendersTheConversationAndJoins() {
        composeRule.setContent {
            CompositionLocalProvider(LocalAppGraph provides harness.graph) {
                GroupDmScreen(
                    roomJid = ROOM,
                    name = "Alice, Bob",
                    onBack = {},
                    onOpenThread = {},
                    onOpenMembers = {},
                )
            }
        }

        composeRule.waitUntil(timeoutMillis = 5_000) {
            composeRule.onAllNodesWithTag(GroupDmTestTags.MEMBERS_ACTION)
                .fetchSemanticsNodes().isNotEmpty()
        }
        // The conversation infra joined the room like a channel would.
        composeRule.waitUntil(timeoutMillis = 5_000) {
            harness.activeFakeClient().joinRoomCalls.any { it.first == ROOM }
        }
        assertTrue(harness.activeFakeClient().joinRoomCalls.any { it.first == ROOM })
    }

    private companion object {
        const val ROOM = "gdm-1@muc.waddle.test"
    }
}
