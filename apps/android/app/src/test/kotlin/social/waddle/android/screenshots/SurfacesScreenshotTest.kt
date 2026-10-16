package social.waddle.android.screenshots

import android.app.Application
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import social.waddle.android.client.prefs.ThemeMode
import social.waddle.android.client.push.PushStatus
import social.waddle.android.client.store.MemberListStatus
import social.waddle.android.feature.conversation.MessageActionList
import social.waddle.android.feature.conversation.MessageSheetActions
import social.waddle.android.feature.members.BotRow
import social.waddle.android.feature.members.MemberList
import social.waddle.android.feature.members.MemberRow
import social.waddle.android.feature.members.MembersUiState
import social.waddle.android.feature.settings.DeliveryUiState
import social.waddle.android.feature.settings.SettingsActions
import social.waddle.android.feature.settings.SettingsContent
import social.waddle.android.feature.settings.SettingsUiState
import social.waddle.client.ffi.WaddleMucAffiliation

/** Secondary surfaces: settings, members and the message action sheet. */
@RunWith(AndroidJUnit4::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [35], qualifiers = "w411dp-h891dp-xxhdpi", application = Application::class)
class SurfacesScreenshotTest {
    @Test
    fun settings() = captureThemed("settings") {
        SettingsContent(
            delivery = DeliveryUiState(
                status = PushStatus.ACTIVE,
                distributorLabel = "ntfy",
                distributorInstalled = true,
                stayConnected = false,
            ),
            state = SettingsUiState(
                username = "david",
                jid = "david@waddle.test",
                theme = ThemeMode.NIGHT,
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

    @Test
    fun members() = captureThemed("members") {
        MemberList(
            state = MembersUiState(
                status = MemberListStatus.LOADED,
                rows = listOf(
                    member("David", WaddleMucAffiliation.OWNER, present = true, hats = listOf("Maintainer")),
                    member("Alice", WaddleMucAffiliation.ADMIN, present = true),
                    member("Bob", WaddleMucAffiliation.MEMBER),
                    member("Mallory", WaddleMucAffiliation.OUTCAST),
                ),
                bots = listOf(BotRow(jid = "herald@bots.waddle.test", displayName = "Herald")),
            ),
            onRowClick = {},
        )
    }

    private fun member(
        name: String,
        affiliation: WaddleMucAffiliation,
        present: Boolean = false,
        hats: List<String> = emptyList(),
    ) = MemberRow(
        jid = "${name.lowercase()}@waddle.test",
        displayName = name,
        affiliation = affiliation,
        nick = name.lowercase(),
        presentNow = present,
        hats = hats,
        inferred = false,
    )
}
