package social.waddle.android.feature.call

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CallEnd
import androidx.compose.material.icons.filled.Cameraswitch
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material.icons.filled.Mic
import androidx.compose.material.icons.filled.MicOff
import androidx.compose.material.icons.filled.Videocam
import androidx.compose.material.icons.filled.VideocamOff
import androidx.compose.material.icons.filled.VolumeUp
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.FilledTonalIconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import social.waddle.android.R
import social.waddle.android.client.calls.CallState
import social.waddle.android.jid.bareJidOf
import social.waddle.android.jid.localpartOf

/**
 * The in-call surface for outgoing-ring and active phases. Audio
 * calls render avatar + status + controls; video calls put the remote
 * track full-bleed with the local preview picture-in-picture.
 */
@Composable
fun ActiveCallScreen(
    state: CallState,
    viewModel: CallViewModel,
    onMinimize: () -> Unit,
) {
    val peerJid = when (state) {
        is CallState.Outgoing -> state.to
        is CallState.Active -> state.peer
        else -> return
    }
    val isVideoCall = when (state) {
        is CallState.Outgoing -> state.media.video
        is CallState.Active -> state.media.video
        else -> false
    }
    val durationSeconds by viewModel.durationSeconds.collectAsStateWithLifecycle()
    val remoteVideo by viewModel.media.remoteVideo.collectAsStateWithLifecycle()
    val localVideo by viewModel.media.localVideo.collectAsStateWithLifecycle()
    val micEnabled by viewModel.media.micEnabled.collectAsStateWithLifecycle()
    val cameraEnabled by viewModel.media.cameraEnabled.collectAsStateWithLifecycle()
    val speakerOn by viewModel.media.speakerOn.collectAsStateWithLifecycle()

    Surface(
        modifier = Modifier
            .fillMaxSize()
            .testTag(CallTestTags.ACTIVE_SCREEN),
        color = MaterialTheme.colorScheme.surface,
    ) {
        Box(modifier = Modifier.fillMaxSize()) {
            if (isVideoCall && remoteVideo != null) {
                CallVideoView(
                    handle = remoteVideo ?: return@Box,
                    modifier = Modifier.fillMaxSize(),
                )
            } else {
                AudioCallBackdrop(
                    peerJid = peerJid,
                    statusText = callStatusText(state, durationSeconds),
                )
            }

            if (isVideoCall) {
                localVideo?.let { handle ->
                    CallVideoView(
                        handle = handle,
                        modifier = Modifier
                            .align(Alignment.TopEnd)
                            .padding(16.dp)
                            .width(112.dp)
                            .aspectRatio(LOCAL_PREVIEW_ASPECT)
                            .clip(RoundedCornerShape(12.dp)),
                    )
                }
            }

            IconButton(
                onClick = onMinimize,
                modifier = Modifier
                    .align(Alignment.TopStart)
                    .padding(8.dp),
            ) {
                Icon(
                    Icons.Filled.ExpandMore,
                    contentDescription = stringResource(R.string.call_action_minimize),
                )
            }

            CallControls(
                isVideoCall = isVideoCall,
                micEnabled = micEnabled,
                cameraEnabled = cameraEnabled,
                speakerOn = speakerOn,
                onToggleMic = viewModel::toggleMic,
                onToggleCamera = viewModel::toggleCamera,
                onFlipCamera = viewModel::flipCamera,
                onToggleSpeaker = viewModel::toggleSpeaker,
                onHangUp = viewModel::hangUp,
                modifier = Modifier
                    .align(Alignment.BottomCenter)
                    .padding(bottom = 40.dp),
            )
        }
    }
}

@Composable
private fun AudioCallBackdrop(peerJid: String, statusText: String) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(top = 96.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        CallPeerAvatar(peerJid = peerJid, size = 112.dp)
        Text(
            text = localpartOf(bareJidOf(peerJid)),
            style = MaterialTheme.typography.headlineMedium,
        )
        Text(
            text = statusText,
            style = MaterialTheme.typography.bodyLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun CallControls(
    isVideoCall: Boolean,
    micEnabled: Boolean,
    cameraEnabled: Boolean,
    speakerOn: Boolean,
    onToggleMic: () -> Unit,
    onToggleCamera: () -> Unit,
    onFlipCamera: () -> Unit,
    onToggleSpeaker: () -> Unit,
    onHangUp: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Row(
        modifier = modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.SpaceEvenly,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        FilledTonalIconButton(onClick = onToggleMic, modifier = Modifier.size(56.dp)) {
            Icon(
                if (micEnabled) Icons.Filled.Mic else Icons.Filled.MicOff,
                contentDescription = stringResource(R.string.call_action_toggle_mic),
            )
        }
        if (isVideoCall) {
            FilledTonalIconButton(onClick = onToggleCamera, modifier = Modifier.size(56.dp)) {
                Icon(
                    if (cameraEnabled) Icons.Filled.Videocam else Icons.Filled.VideocamOff,
                    contentDescription = stringResource(R.string.call_action_toggle_camera),
                )
            }
            FilledTonalIconButton(onClick = onFlipCamera, modifier = Modifier.size(56.dp)) {
                Icon(
                    Icons.Filled.Cameraswitch,
                    contentDescription = stringResource(R.string.call_action_flip_camera),
                )
            }
        }
        FilledTonalIconButton(
            onClick = onToggleSpeaker,
            modifier = Modifier.size(56.dp),
            colors = if (speakerOn) {
                IconButtonDefaults.filledTonalIconButtonColors(
                    containerColor = MaterialTheme.colorScheme.primaryContainer,
                )
            } else {
                IconButtonDefaults.filledTonalIconButtonColors()
            },
        ) {
            Icon(
                Icons.Filled.VolumeUp,
                contentDescription = stringResource(R.string.call_action_toggle_speaker),
            )
        }
        FilledIconButton(
            onClick = onHangUp,
            modifier = Modifier
                .size(64.dp)
                .testTag(CallTestTags.HANG_UP_BUTTON),
            colors = IconButtonDefaults.filledIconButtonColors(
                containerColor = MaterialTheme.colorScheme.error,
                contentColor = MaterialTheme.colorScheme.onError,
            ),
        ) {
            Icon(
                Icons.Filled.CallEnd,
                contentDescription = stringResource(R.string.call_action_hang_up),
            )
        }
    }
}

/** Circular monogram avatar for the call surfaces. */
@Composable
internal fun CallPeerAvatar(peerJid: String, size: Dp) {
    val name = localpartOf(bareJidOf(peerJid))
    Box(
        modifier = Modifier
            .size(size)
            .clip(CircleShape)
            .background(MaterialTheme.colorScheme.primaryContainer),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = name.take(1).uppercase(),
            style = MaterialTheme.typography.displaySmall,
            color = MaterialTheme.colorScheme.onPrimaryContainer,
        )
    }
}

@Composable
internal fun callStatusText(state: CallState, durationSeconds: Long): String = when (state) {
    is CallState.Outgoing -> stringResource(
        if (state.ringing) R.string.call_status_ringing else R.string.call_status_calling,
    )
    is CallState.Active -> formatCallDuration(durationSeconds)
    else -> ""
}

/** `m:ss` under an hour, `h:mm:ss` above (web call-duration parity). */
internal fun formatCallDuration(totalSeconds: Long): String {
    val hours = totalSeconds / SECONDS_PER_HOUR
    val minutes = (totalSeconds % SECONDS_PER_HOUR) / SECONDS_PER_MINUTE
    val seconds = totalSeconds % SECONDS_PER_MINUTE
    return if (hours > 0) {
        "%d:%02d:%02d".format(hours, minutes, seconds)
    } else {
        "%d:%02d".format(minutes, seconds)
    }
}

private const val LOCAL_PREVIEW_ASPECT = 3f / 4f
private const val SECONDS_PER_MINUTE = 60L
private const val SECONDS_PER_HOUR = 3_600L

/** Semantics tags shared with the instrumented call tests. */
object CallTestTags {
    const val INCOMING_SCREEN = "call-incoming-screen"
    const val ACTIVE_SCREEN = "call-active-screen"
    const val ACCEPT_BUTTON = "call-accept"
    const val DECLINE_BUTTON = "call-decline"
    const val HANG_UP_BUTTON = "call-hang-up"
    const val BANNER = "call-banner"
    const val ENDED_BANNER = "call-ended-banner"
    const val FAKE_VIDEO_SURFACE = "call-fake-video"
}
