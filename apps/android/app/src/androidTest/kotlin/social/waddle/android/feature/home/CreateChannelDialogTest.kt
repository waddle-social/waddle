package social.waddle.android.feature.home

import androidx.activity.ComponentActivity
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import social.waddle.android.LocalAppGraph
import social.waddle.android.TestAppGraph
import social.waddle.client.ffi.WaddleClientEvent

/**
 * Create-channel flow end-to-end through the faked FFI: the owner-only
 * "+" affordance opens the dialog, and submitting drives the XEP-0045
 * §10.1 create verb with the slugged localpart and the typed config
 * patch.
 */
@RunWith(AndroidJUnit4::class)
class CreateChannelDialogTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private lateinit var harness: TestAppGraph
    private val openedChannels = mutableListOf<Pair<String, String>>()

    @Before
    fun setUp() {
        harness = TestAppGraph()
        harness.signIn()
        composeRule.waitUntil(timeoutMillis = 10_000) {
            harness.clientFactory.clients.isNotEmpty()
        }
        // The owner probe fires once the connection is Ready; the
        // canned answer must be in place before that.
        harness.activeFakeClient().communityOwner = true
        harness.clientFactory.emit(WaddleClientEvent.Connected)

        composeRule.setContent {
            CompositionLocalProvider(LocalAppGraph provides harness.graph) {
                HomeScreen(
                    onOpenChannel = { roomJid, name -> openedChannels += roomJid to name },
                    onOpenDmList = {},
                    onOpenSettings = {},
                )
            }
        }
    }

    @After
    fun tearDown() {
        harness.shutdown()
    }

    @Test
    fun creatingAChannelSubmitsTheTypedConfigThroughTheFake() {
        composeRule.waitUntil(timeoutMillis = 10_000) {
            composeRule.onAllNodes(
                androidx.compose.ui.test.hasTestTag(HomeScreenTestTags.CREATE_CHANNEL_ACTION),
            ).fetchSemanticsNodes().isNotEmpty()
        }
        composeRule.onNodeWithTag(HomeScreenTestTags.CREATE_CHANNEL_ACTION).performClick()

        composeRule.onNodeWithTag(CreateChannelDialogTestTags.NAME_FIELD)
            .performTextInput("Design Reviews")
        composeRule.onNodeWithTag(CreateChannelDialogTestTags.DESCRIPTION_FIELD)
            .performTextInput("weekly crits")
        composeRule.onNodeWithTag(CreateChannelDialogTestTags.FORUM_OPTION).performClick()
        composeRule.onNodeWithTag(CreateChannelDialogTestTags.CREATE_BUTTON).performClick()

        composeRule.waitUntil(timeoutMillis = 10_000) {
            harness.activeFakeClient().createRoomCalls.isNotEmpty()
        }
        val (localpart, nick, patch) = harness.activeFakeClient().createRoomCalls.single()
        assertEquals("design-reviews", localpart)
        assertEquals("icepuma", nick)
        assertEquals("Design Reviews", patch.name)
        assertEquals("weekly crits", patch.description)
        assertEquals(true, patch.forum)

        // Success navigates into the freshly created channel.
        composeRule.waitUntil(timeoutMillis = 10_000) { openedChannels.isNotEmpty() }
        assertEquals("design-reviews@muc.waddle.test", openedChannels.single().first)
    }
}
