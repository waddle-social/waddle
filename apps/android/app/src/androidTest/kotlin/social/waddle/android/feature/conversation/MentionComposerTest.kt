package social.waddle.android.feature.conversation

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.hasSetTextAction
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import social.waddle.android.TestAppGraph
import social.waddle.android.client.sendCalls
import social.waddle.android.client.sendOptions
import social.waddle.android.client.testPresence
import social.waddle.android.feature.channel.ChannelViewModel
import social.waddle.client.ffi.WaddleMucRole
import social.waddle.client.ffi.WaddleReferenceType

/**
 * XEP-0372 mentions end-to-end through the faked FFI: an occupant is
 * seeded via a presence event, `@` opens the popover, selecting the
 * candidate inserts the label, and the send carries a typed mention
 * reference with code-point offsets.
 */
@RunWith(AndroidJUnit4::class)
class MentionComposerTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private lateinit var harness: TestAppGraph
    private lateinit var viewModel: ChannelViewModel

    @Before
    fun setUp() {
        harness = TestAppGraph()
        harness.signInAndConnect()
        harness.emitPresence(
            testPresence(
                from = "$ROOM_JID/bob",
                mucRole = WaddleMucRole.PARTICIPANT,
                mucJid = "bob@waddle.test/phone",
            ),
        )
        viewModel = ChannelViewModel(
            sessionManager = harness.graph.sessionManager,
            roomJid = ROOM_JID,
            nick = "icepuma",
        )
        composeRule.setContent {
            ConversationScreen(
                title = "general",
                viewModel = viewModel,
                onBack = {},
                onOpenThread = null,
            )
        }
    }

    @After
    fun tearDown() {
        harness.shutdown()
    }

    @Test
    fun typingAtOpensThePopoverAndSendCarriesTheMentionReference() {
        composeRule.onNode(hasSetTextAction()).performTextInput("hi @")
        composeRule.waitUntil(timeoutMillis = 10_000) {
            composeRule.onAllNodesWithText("@bob").fetchSemanticsNodes().isNotEmpty()
        }

        composeRule.onNodeWithText("@bob").performClick()
        composeRule.onNodeWithContentDescription("Send").performClick()

        composeRule.waitUntil(timeoutMillis = 10_000) {
            harness.activeFakeClient().sendCalls.contains(ROOM_JID to "hi @bob")
        }
        val options = harness.activeFakeClient().sendOptions.last()
        val reference = checkNotNull(options).references.single()
        assertEquals(WaddleReferenceType.Mention, reference.refType)
        assertEquals("xmpp:bob@waddle.test", reference.uri)
        assertEquals(3u, reference.begin)
        assertEquals(7u, reference.end)
        assertNull(reference.anchor)
    }

    private companion object {
        const val ROOM_JID = "general@muc.waddle.test"
    }
}
