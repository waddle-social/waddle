package social.waddle.android.feature.conversation

import android.app.Application
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.test.swipe
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import social.waddle.android.client.prefs.ThemeMode
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.TimelineSource
import social.waddle.android.client.testMessage
import social.waddle.android.theme.WaddleTheme
import java.time.ZoneOffset

/** Real message rows reply after a deliberate swipe and ignore short drags. */
@RunWith(AndroidJUnit4::class)
@Config(sdk = [35], qualifiers = "w411dp-h891dp-xxhdpi", application = Application::class)
class SwipeToReplyTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun `a short drag does not reply but a full swipe targets the message`() {
        val item = TimelineItem(
            id = "message-1",
            conversationJid = "alice@waddle.test",
            from = "alice@waddle.test",
            body = "Reply to this message",
            timestamp = "2026-07-15T10:00:00Z",
            isMine = false,
            source = TimelineSource.Live(testMessage(id = "message-1", from = "alice@waddle.test")),
        )
        val entry = feedEntriesOf(
            items = listOf(item),
            pending = emptyList(),
            context = FeedContext(
                threadId = null,
                selfBareJid = "me@waddle.test",
                pinnedIds = emptySet(),
                unreadAtOpen = 0,
                zone = ZoneOffset.UTC,
            ),
        ).single()
        val replies = mutableListOf<TimelineItem>()
        compose.setContent {
            WaddleTheme(themeMode = ThemeMode.DAYLIGHT) {
                MessageRow(
                    entry = entry,
                    actions = MessageRowActions(onReply = { replies += it }),
                    context = MessageRowContext(),
                    modifier = Modifier.testTag("reply-row"),
                )
            }
        }
        val row = compose.onNodeWithTag("reply-row")
        row.performTouchInput {
            swipe(Offset(width * 0.1f, centerY), Offset(width * 0.2f, centerY))
        }
        compose.runOnIdle { assertEquals(emptyList<TimelineItem>(), replies) }
        row.performTouchInput {
            swipe(Offset(width * 0.1f, centerY), Offset(width * 0.8f, centerY))
        }
        compose.runOnIdle { assertEquals(listOf(item), replies) }
    }
}
