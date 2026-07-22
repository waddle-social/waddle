package social.waddle.android.feature.conversation

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import social.waddle.android.client.store.TimelineStore
import social.waddle.android.client.testMessage

/**
 * Negative gating for the XEP-0425 action: "Delete for everyone" is
 * offered only to moderators looking at ANOTHER user's message. The
 * composable branch is the UI-side gate, so it gets pinned directly.
 */
@RunWith(AndroidJUnit4::class)
class MessageActionSheetModerationTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private fun itemFrom(nick: String): social.waddle.android.client.store.TimelineItem {
        val store = TimelineStore().apply { setOwnBareJid(OWN_JID) }
        store.onLiveMessage(
            testMessage(
                id = "id-1",
                stanzaId = "s1",
                stanzaIdBy = ROOM,
                from = "$ROOM/$nick",
                to = null,
                messageType = "groupchat",
                isMuc = true,
            ),
        )
        return store.timeline(ROOM).value.single()
    }

    private fun setSheetContent(
        item: social.waddle.android.client.store.TimelineItem,
        canModerate: Boolean,
    ) {
        composeRule.setContent {
            MessageActionSheet(
                item = item,
                actionable = true,
                canPin = false,
                isPinned = false,
                canModerate = canModerate,
                onDismiss = {},
                onReact = {},
                onReply = {},
                onReplyInThread = null,
                onEdit = {},
                onRetract = {},
                onCopy = {},
                onSetPinned = {},
                onModerate = {},
            )
        }
    }

    @Test
    fun moderateActionHiddenWithoutModeratorPrivileges() {
        setSheetContent(itemFrom("mallory"), canModerate = false)
        composeRule.onNodeWithText("Copy text").assertExists()
        composeRule.onNodeWithText("Delete for everyone").assertDoesNotExist()
    }

    @Test
    fun moderateActionShownToModeratorsForOthersMessages() {
        setSheetContent(itemFrom("mallory"), canModerate = true)
        composeRule.onNodeWithText("Delete for everyone").assertExists()
    }

    @Test
    fun moderateActionHiddenForOwnMessagesEvenAsModerator() {
        val ownItem = itemFrom("icepuma")
        check(ownItem.isMine)
        setSheetContent(ownItem, canModerate = true)
        composeRule.onNodeWithText("Delete for everyone").assertDoesNotExist()
        // Own rows keep the XEP-0424 retract action instead.
        composeRule.onNodeWithText("Delete").assertExists()
    }

    private companion object {
        const val ROOM = "general@muc.waddle.test"
        const val OWN_JID = "icepuma@waddle.test"
    }
}
