package social.waddle.android.feature.conversation

import android.app.Application
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotDisplayed
import androidx.compose.ui.test.hasScrollAction
import androidx.compose.ui.test.junit4.StateRestorationTester
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performScrollToIndex
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.test.swipe
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import social.waddle.android.client.prefs.ThemeMode
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.TimelineSource
import social.waddle.android.client.testMessage
import social.waddle.android.theme.LocalConsistentHue
import social.waddle.android.theme.WaddleTheme
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneOffset
import java.util.Locale
import java.util.TimeZone

/** The timeline follows new messages for a reader at the newest edge. */
@RunWith(AndroidJUnit4::class)
@Config(sdk = [35], qualifiers = "w411dp-h891dp-xxhdpi", application = Application::class)
class TimelineFollowTest {
    @get:Rule
    val compose = createComposeRule()

    private val room = "room@muc.waddle.test"
    private val self = "me@waddle.test"

    private fun item(n: Int) = TimelineItem(
        id = "m$n",
        conversationJid = room,
        from = "$room/alice",
        body = "message $n",
        timestamp = "2026-07-15T10:%02d:00Z".format(n),
        isMine = false,
        source = TimelineSource.Live(
            testMessage(id = "m$n", from = "$room/alice", messageType = "groupchat", isMuc = true),
        ),
        authorJid = "alice@waddle.test",
    )

    private fun entries(items: List<TimelineItem>, pending: List<PendingMessage> = emptyList()) = feedEntriesOf(
        items = items,
        pending = pending,
        context = FeedContext(
            threadId = null,
            selfBareJid = self,
            pinnedIds = emptySet(),
            unreadAtOpen = 0,
            zone = ZoneOffset.UTC,
        ),
    )

    private val history = (1..40).map(::item)
    private val shown = mutableStateOf(entries(history))

    private val sent = PendingMessage(
        localId = 1,
        stanzaId = null,
        body = "my reply",
        timestampMillis = Instant.parse("2026-07-15T11:00:00Z").toEpochMilli(),
        failed = false,
    )

    /** The server's echo of [sent]: a stored row with a new key. */
    private val echo = item(41).copy(id = "echo", body = "my reply", from = "$room/me", isMine = true)

    private lateinit var savedLocale: Locale
    private lateinit var savedZone: TimeZone

    // Timestamps format with the JVM defaults; pin them like the
    // screenshot tests so test order cannot change either's output.
    @Before
    fun pinLocale() {
        savedLocale = Locale.getDefault()
        savedZone = TimeZone.getDefault()
        Locale.setDefault(Locale.UK)
        TimeZone.setDefault(TimeZone.getTimeZone("UTC"))
    }

    @After
    fun restoreLocale() {
        Locale.setDefault(savedLocale)
        TimeZone.setDefault(savedZone)
    }

    @Composable
    private fun Timeline() {
        CompositionLocalProvider(LocalConsistentHue provides { 0.0 }) {
            WaddleTheme(themeMode = ThemeMode.DAYLIGHT) {
                TimelineList(
                    entries = shown.value,
                    isLoadingOlder = false,
                    reachedHistoryStart = true,
                    onTopReached = {},
                    actions = MessageRowActions(),
                    context = MessageRowContext(selfBareJid = self),
                    today = LocalDate.of(2026, 7, 15),
                    modifier = Modifier.fillMaxSize(),
                )
            }
        }
    }

    private fun showTimeline() {
        compose.setContent { Timeline() }
        compose.onNodeWithText("message 40").assertIsDisplayed()
    }

    @Test
    fun `a reader at the newest message sees the next one arrive`() {
        showTimeline()

        shown.value = entries(history + item(41))

        compose.onNodeWithText("message 41").assertIsDisplayed()
    }

    @Test
    fun `a reader nudged just off the bottom still follows new messages`() {
        showTimeline()
        // A slow drag of a few dozen pixels: the newest message is still on
        // screen, just no longer flush with the bottom edge.
        compose.onNode(hasScrollAction()).performTouchInput {
            swipe(start = center, end = center + Offset(0f, NUDGE_PX), durationMillis = 600)
        }
        compose.onNodeWithText("message 40").assertIsDisplayed()

        shown.value = entries(history + item(41))

        compose.onNodeWithText("message 41").assertIsDisplayed()
    }

    @Test
    fun `a reader scrolled back through history is not pulled down`() {
        showTimeline()
        compose.onNode(hasScrollAction()).performScrollToIndex(35)
        compose.onNodeWithText("message 5").assertIsDisplayed()

        shown.value = entries(history + item(41))

        compose.onNodeWithText("message 5").assertIsDisplayed()
        compose.onNodeWithText("message 41").assertIsNotDisplayed()
    }

    @Test
    fun `sending jumps to the newest message from anywhere`() {
        showTimeline()
        compose.onNode(hasScrollAction()).performScrollToIndex(35)

        shown.value = entries(history, pending = listOf(sent))

        compose.onNodeWithText("my reply").assertIsDisplayed()
    }

    @Test
    fun `the echo replacing a send does not stop the jump to it`() {
        showTimeline()
        compose.onNode(hasScrollAction()).performScrollToIndex(35)

        compose.mainClock.autoAdvance = false
        shown.value = entries(history, pending = listOf(sent))
        repeat(JUMP_FRAMES_BEFORE_ECHO) { compose.mainClock.advanceTimeByFrame() }
        shown.value = entries(history + echo)
        compose.mainClock.autoAdvance = true

        compose.onNodeWithText("my reply").assertIsDisplayed()
    }

    @Test
    fun `a pending send already there on restore keeps the reader's place`() {
        val restoration = StateRestorationTester(compose)
        shown.value = entries(history, pending = listOf(sent))
        restoration.setContent { Timeline() }
        compose.onNode(hasScrollAction()).performScrollToIndex(36)
        compose.onNodeWithText("message 5").assertIsDisplayed()

        restoration.emulateSavedInstanceStateRestore()

        compose.onNodeWithText("message 5").assertIsDisplayed()
        compose.onNodeWithText("my reply").assertIsNotDisplayed()
    }

    private companion object {
        const val NUDGE_PX = 60f

        /** Long enough to be mid-jump from deep history, well short of its end. */
        const val JUMP_FRAMES_BEFORE_ECHO = 3
    }
}
