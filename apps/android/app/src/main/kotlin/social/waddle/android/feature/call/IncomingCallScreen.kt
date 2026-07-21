package social.waddle.android.feature.call

import android.Manifest
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Call
import androidx.compose.material.icons.filled.CallEnd
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import social.waddle.android.R
import social.waddle.android.client.calls.CallState
import social.waddle.android.jid.bareJidOf
import social.waddle.android.jid.localpartOf

/**
 * Full-screen incoming ring: caller identity, call kind, and the
 * answer/decline pair. Answering requests the RECORD_AUDIO (and, for
 * video calls, CAMERA) runtime permission BEFORE the XEP-0353
 * `<proceed/>` goes out; a denial still accepts receive-only, the
 * same best-effort capture posture as the web client.
 */
@Composable
fun IncomingCallScreen(
    state: CallState.Incoming,
    autoAnswer: Boolean,
    onAutoAnswerConsumed: () -> Unit,
    onAccept: () -> Unit,
    onDecline: () -> Unit,
) {
    val permissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions(),
    ) { onAccept() }
    val permissions = if (state.media.video) {
        arrayOf(Manifest.permission.RECORD_AUDIO, Manifest.permission.CAMERA)
    } else {
        arrayOf(Manifest.permission.RECORD_AUDIO)
    }

    LaunchedEffect(autoAnswer, state.sid) {
        if (autoAnswer && !state.accepting) {
            onAutoAnswerConsumed()
            permissionLauncher.launch(permissions)
        }
    }

    // Back must not dismiss the ring into the hidden nav stack — the
    // user answers or declines explicitly.
    BackHandler {}

    Surface(
        modifier = Modifier
            .fillMaxSize()
            .testTag(CallTestTags.INCOMING_SCREEN),
        color = MaterialTheme.colorScheme.surface,
    ) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(32.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.SpaceBetween,
        ) {
            Column(
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(8.dp),
                modifier = Modifier.padding(top = 64.dp),
            ) {
                CallPeerAvatar(peerJid = state.from, size = 96.dp)
                Text(
                    text = localpartOf(bareJidOf(state.from)),
                    style = MaterialTheme.typography.headlineMedium,
                )
                Text(
                    text = stringResource(
                        if (state.media.video) R.string.call_incoming_video else R.string.call_incoming_audio,
                    ),
                    style = MaterialTheme.typography.bodyLarge,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                if (state.accepting) {
                    CircularProgressIndicator(modifier = Modifier.padding(top = 16.dp))
                    Text(
                        text = stringResource(R.string.call_status_connecting),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(bottom = 48.dp),
                horizontalArrangement = Arrangement.SpaceEvenly,
            ) {
                FilledIconButton(
                    onClick = onDecline,
                    modifier = Modifier
                        .size(72.dp)
                        .testTag(CallTestTags.DECLINE_BUTTON),
                    colors = IconButtonDefaults.filledIconButtonColors(
                        containerColor = MaterialTheme.colorScheme.error,
                        contentColor = MaterialTheme.colorScheme.onError,
                    ),
                ) {
                    Icon(
                        Icons.Filled.CallEnd,
                        contentDescription = stringResource(R.string.call_action_decline),
                    )
                }
                FilledIconButton(
                    onClick = { permissionLauncher.launch(permissions) },
                    enabled = !state.accepting,
                    modifier = Modifier
                        .size(72.dp)
                        .testTag(CallTestTags.ACCEPT_BUTTON),
                ) {
                    Icon(
                        Icons.Filled.Call,
                        contentDescription = stringResource(R.string.call_action_accept),
                    )
                }
            }
        }
    }
}
