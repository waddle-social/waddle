package social.waddle.android.screenshots

import android.app.Application
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import social.waddle.android.client.prefs.ThemeMode
import social.waddle.android.feature.conversation.MessageActionList
import social.waddle.android.feature.conversation.MessageSheetActions
import social.waddle.android.feature.settings.SettingsActions
import social.waddle.android.feature.settings.SettingsContent
import social.waddle.android.feature.settings.SettingsUiState

/** Secondary surfaces: settings and the message action sheet. */
@RunWith(AndroidJUnit4::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [35], qualifiers = "w411dp-h891dp-xxhdpi", application = Application::class)
class SurfacesScreenshotTest {
    @Test
    fun settings() = captureThemed("settings") {
        SettingsContent(
            state = SettingsUiState(
                username = "david",
                jid = "david@waddle.test",
                theme = ThemeMode.SYSTEM,
                messageSoundsEnabled = false,
            ),
            selfAvatar = null,
            isCommunityOwner = true,
            actions = SettingsActions(
                onBack = {},
                onOpenProfile = {},
                onOpenCommunityUsers = {},
                onSetTheme = {},
                onSetNotificationsEnabled = {},
                onSetMessageSoundsEnabled = {},
                onSetReadReceiptsEnabled = {},
                onLogout = {},
            ),
        )
    }

    @Test
    fun messageActions() = captureThemed("message_actions") {
        MessageActionList(
            item = ScreenshotFixtures.otherMessage(),
            actionable = true,
            canPin = true,
            isPinned = false,
            canModerate = true,
            actions = MessageSheetActions(
                onDismiss = {},
                onReact = {},
                onReply = {},
                onReplyInThread = {},
                onEdit = {},
                onRetract = {},
                onCopy = {},
                onSetPinned = {},
                onModerate = {},
            ),
        )
    }
}
