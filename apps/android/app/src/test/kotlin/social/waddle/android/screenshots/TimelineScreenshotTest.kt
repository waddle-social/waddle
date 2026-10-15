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
import social.waddle.android.feature.conversation.TimelineList
import java.util.Locale
import java.util.TimeZone

/**
 * Golden screenshots of the conversation timeline, light and dark.
 * Re-record after an intended change: `./gradlew :app:recordRoborazziDebug`.
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
        TimelineList(
            entries = ScreenshotFixtures.channelEntries(),
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
