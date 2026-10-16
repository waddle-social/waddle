package social.waddle.android.screenshots

import android.app.Application
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import social.waddle.android.client.calls.CallEndReason
import social.waddle.android.feature.call.ActiveCallActions
import social.waddle.android.feature.call.ActiveCallContent
import social.waddle.android.feature.call.ActiveCallUi
import social.waddle.android.feature.call.CallEndedBanner
import social.waddle.android.feature.call.CallPhase
import social.waddle.android.feature.call.ChannelCallBanner
import social.waddle.android.feature.call.ChannelCallBannerState
import social.waddle.android.feature.call.IncomingCallContent
import social.waddle.android.feature.call.MucRosterEntry
import social.waddle.android.feature.call.OngoingCallBanner
import social.waddle.android.feature.call.VideoTrackHandle
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.WaddleTopBar
import social.waddle.android.ui.huddle.LocalReducedMotion
import social.waddle.client.ffi.WaddleJingleReason

/**
 * Calls: the night stage (group huddle with a speaker, a raised hand
 * and muted people; a DM call; an outgoing ring; video), the incoming
 * ring, and the banners. Recorded in both app themes: the stage is
 * night either way, the banners sit over the app's own ground.
 */
@RunWith(AndroidJUnit4::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [35], qualifiers = "w411dp-h891dp-xxhdpi", application = Application::class)
class CallScreenshotTest {
    private val huddle = listOf(
        MucRosterEntry(nick = "david", handRaised = false, muted = false, jid = "david@waddle.test", self = true),
        MucRosterEntry(nick = "mara", handRaised = false, muted = false, jid = "mara@waddle.test", speaking = true),
        MucRosterEntry(nick = "sol", handRaised = true, muted = false, jid = "sol@waddle.test"),
        MucRosterEntry(nick = "kai", handRaised = false, muted = true, jid = "kai@waddle.test"),
        MucRosterEntry(nick = "priya", handRaised = true, muted = true, jid = "priya@waddle.test"),
        MucRosterEntry(nick = "theodora", handRaised = false, muted = false, jid = "theodora@waddle.test"),
    )

    @Test
    fun activeGroupCall() = captureCall("call_active_group") {
        ActiveCallContent(
            ui = ActiveCallUi(
                title = "general",
                avatarJid = null,
                group = true,
                video = false,
                phase = CallPhase.Live(seconds = 754),
                roster = huddle,
                micEnabled = true,
                speakerOn = false,
                handRaised = false,
            ),
            actions = ActiveCallActions(),
        )
    }

    @Test
    fun activeDmCall() = captureCall("call_active_dm") {
        ActiveCallContent(
            ui = ActiveCallUi(
                title = "mara",
                avatarJid = "mara@waddle.test",
                group = false,
                video = false,
                phase = CallPhase.Live(seconds = 3_725),
                peerSpeaking = true,
                micEnabled = false,
                speakerOn = true,
            ),
            actions = ActiveCallActions(),
        )
    }

    @Test
    fun outgoingCall() = captureCall("call_outgoing") {
        ActiveCallContent(
            ui = ActiveCallUi(
                title = "sol",
                avatarJid = "sol@waddle.test",
                group = false,
                video = false,
                phase = CallPhase.Ringing,
            ),
            actions = ActiveCallActions(),
        )
    }

    @Test
    fun activeVideoCall() = captureCall("call_active_video") {
        ActiveCallContent(
            ui = ActiveCallUi(
                title = "kai",
                avatarJid = "kai@waddle.test",
                group = false,
                video = true,
                phase = CallPhase.Live(seconds = 95),
                cameraEnabled = true,
                speakerOn = true,
            ),
            actions = ActiveCallActions(),
            remoteVideo = FakeTrack,
            localVideo = FakeTrack,
        )
    }

    @Test
    fun incomingCall() = captureCall("call_incoming") {
        IncomingCallContent(
            callerJid = "mara@waddle.test",
            video = false,
            accepting = false,
            onAnswer = {},
            onDecline = {},
        )
    }

    @Test
    fun banners() = captureCall("call_banners") {
        Column(modifier = Modifier.fillMaxSize(), verticalArrangement = Arrangement.spacedBy(Waddle.space.md)) {
            Column {
                OngoingCallBanner(
                    title = "general",
                    group = true,
                    phase = CallPhase.Live(seconds = 754),
                    onExpand = {},
                    onHangUp = {},
                )
                ConversationTopBar("general")
                ChannelCallBanner(
                    banner = ChannelCallBannerState.Ongoing(participantCount = 4),
                    onJoin = {},
                )
            }
            Column {
                CallEndedBanner(reason = CallEndReason.Finished(WaddleJingleReason.CONNECTIVITY_ERROR), onDismiss = {})
                ConversationTopBar("design")
                ChannelCallBanner(
                    banner = ChannelCallBannerState.Join(
                        participantCount = 5,
                        nicks = listOf("mara", "sol", "kai", "priya", "theodora"),
                        video = false,
                        busy = false,
                    ),
                    onJoin = {},
                )
                ChannelCallBanner(
                    banner = ChannelCallBannerState.Join(
                        participantCount = 2,
                        nicks = listOf("mara", "sol"),
                        video = true,
                        busy = true,
                    ),
                    onJoin = {},
                )
                ChannelCallBanner(
                    banner = ChannelCallBannerState.LeaveRetained(participantCount = 2),
                    onJoin = {},
                )
            }
        }
    }

    /** The conversation's top bar, to show the banner sits above it and covers nothing. */
    @Composable
    private fun ConversationTopBar(room: String) {
        WaddleTopBar(
            title = { Text(room) },
            navigationIcon = {
                IconButton(onClick = {}) { Icon(Icons.AutoMirrored.Outlined.ArrowBack, contentDescription = null) }
            },
        )
    }

    private fun captureCall(name: String, reducedMotion: Boolean = true, content: @Composable () -> Unit) =
        captureThemed(name) {
            // Loops held still so the goldens are deterministic.
            CompositionLocalProvider(LocalReducedMotion provides reducedMotion) { content() }
        }

    private object FakeTrack : VideoTrackHandle
}
