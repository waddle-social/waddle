package social.waddle.android.screenshots

import android.app.Application
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.ui.Modifier
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import social.waddle.android.feature.conversation.MessageRowActions
import social.waddle.android.feature.conversation.MessageRowContext
import social.waddle.android.feature.conversation.PendingMessage
import social.waddle.android.feature.conversation.TimelineList
import java.time.Instant
import java.util.Locale
import java.util.TimeZone

/**
 * Golden screenshots of the conversation timeline, light and dark: the
 * shared channel fixture plus a failed and a sending own message, and
 * the skeleton shown while the first page loads. Clock, locale, the
 * 24-hour setting and reduced motion are pinned.
 * Re-record after an intended change: `./gradlew :app:recordRoborazziFossDebug`.
 */
@RunWith(AndroidJUnit4::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [35], qualifiers = "w411dp-h891dp-xxhdpi", application = Application::class)
class TimelineScreenshotTest {
    @Before
    fun pinLocale() {
        Locale.setDefault(Locale.UK)
        TimeZone.setDefault(TimeZone.getTimeZone("UTC"))
    }

    @Test
    fun timeline() = captureThemed("timeline") {
        ConversationFixtures.Pinned {
            TimelineList(
                entries = ScreenshotFixtures.channelEntries(
                    pending = listOf(
                        pending(1, "Dashboards are in the review doc.", "2026-10-16T09:17:00Z", failed = true),
                        pending(2, "And the alert thresholds.", "2026-10-16T09:17:30Z"),
                    ),
                ),
                isLoadingOlder = false,
                reachedHistoryStart = true,
                onTopReached = {},
                actions = MessageRowActions(onOpenThread = {}),
                context = MessageRowContext(selfBareJid = ScreenshotFixtures.SELF),
                today = ScreenshotFixtures.TODAY,
                modifier = Modifier.fillMaxSize(),
            )
        }
    }

    @Test
    fun loading() = captureThemed("timeline_loading") {
        ConversationFixtures.Pinned {
            TimelineList(
                entries = emptyList(),
                isLoadingOlder = true,
                reachedHistoryStart = false,
                onTopReached = {},
                actions = MessageRowActions(),
                context = MessageRowContext(),
                modifier = Modifier.fillMaxSize(),
            )
        }
    }

    private fun pending(id: Long, body: String, at: String, failed: Boolean = false) = PendingMessage(
        localId = id,
        stanzaId = null,
        body = body,
        timestampMillis = Instant.parse(at).toEpochMilli(),
        failed = failed,
    )
}
