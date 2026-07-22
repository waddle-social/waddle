package social.waddle.android.feature.call

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Call
import androidx.compose.material.icons.filled.CallEnd
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.coroutines.flow.StateFlow
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.client.calls.CallState
import social.waddle.android.jid.bareJidOf
import social.waddle.android.jid.localpartOf

/**
 * Global call surface router, layered over the navigation shell:
 * `Incoming` renders the full-screen ring, `Outgoing`/`Active` the
 * in-call screen (or the tap-to-return banner when minimized), and
 * `Ended` a dismissable banner. Driven entirely by the session
 * manager's call slot, so it works no matter which screen is open.
 */
@Composable
fun CallOverlayHost(
    pendingCallAnswer: StateFlow<Boolean>,
    onCallAnswerConsumed: () -> Unit,
) {
    val graph = LocalAppGraph.current
    val viewModel: CallViewModel = viewModel(factory = CallViewModel.factory(graph))
    val state by viewModel.callState.collectAsStateWithLifecycle()
    val minimized by viewModel.minimized.collectAsStateWithLifecycle()
    val autoAnswer by pendingCallAnswer.collectAsStateWithLifecycle()

    // A pending Answer tap whose ring is already gone (retracted,
    // timed out, answered on another device) MUST be consumed, or the
    // stale flag would silently auto-answer the NEXT incoming call.
    LaunchedEffect(autoAnswer, state) {
        if (autoAnswer && state !is CallState.Incoming) {
            onCallAnswerConsumed()
        }
    }

    when (val call = state) {
        is CallState.Incoming -> IncomingCallScreen(
            state = call,
            autoAnswer = autoAnswer,
            onAutoAnswerConsumed = onCallAnswerConsumed,
            onAccept = viewModel::accept,
            onDecline = viewModel::decline,
        )
        is CallState.Outgoing, is CallState.Active ->
            if (minimized) {
                OngoingCallBanner(
                    state = call,
                    onExpand = { viewModel.setMinimized(false) },
                    onHangUp = viewModel::hangUp,
                )
            } else {
                ActiveCallScreen(
                    state = call,
                    viewModel = viewModel,
                    onMinimize = { viewModel.setMinimized(true) },
                )
            }
        is CallState.Ended -> CallEndedBanner(onDismiss = viewModel::dismissEnded)
        CallState.Idle -> Unit
    }
}

/** Compact banner while the in-call surface is minimized. */
@Composable
private fun OngoingCallBanner(
    state: CallState,
    onExpand: () -> Unit,
    onHangUp: () -> Unit,
) {
    val peerJid = when (state) {
        is CallState.Outgoing -> state.to
        is CallState.Active -> state.peer
        else -> return
    }
    Box(modifier = Modifier.fillMaxSize()) {
        Surface(
            color = MaterialTheme.colorScheme.primaryContainer,
            modifier = Modifier
                .align(Alignment.TopCenter)
                .statusBarsPadding()
                .padding(horizontal = 12.dp, vertical = 4.dp)
                .fillMaxWidth()
                .testTag(CallTestTags.BANNER)
                .clickable(onClick = onExpand),
            shape = MaterialTheme.shapes.large,
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
            ) {
                Icon(
                    Icons.Filled.Call,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onPrimaryContainer,
                )
                Text(
                    text = stringResource(
                        R.string.call_banner_ongoing,
                        localpartOf(bareJidOf(peerJid)),
                    ),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onPrimaryContainer,
                    modifier = Modifier
                        .weight(1f)
                        .padding(start = 12.dp),
                )
                IconButton(onClick = onHangUp) {
                    Icon(
                        Icons.Filled.CallEnd,
                        contentDescription = stringResource(R.string.call_action_hang_up),
                        tint = MaterialTheme.colorScheme.error,
                    )
                }
            }
        }
    }
}

/** Terminal-state banner until the user dismisses the slot. */
@Composable
private fun CallEndedBanner(onDismiss: () -> Unit) {
    Box(modifier = Modifier.fillMaxSize()) {
        Surface(
            color = MaterialTheme.colorScheme.surfaceVariant,
            modifier = Modifier
                .align(Alignment.TopCenter)
                .statusBarsPadding()
                .padding(horizontal = 12.dp, vertical = 4.dp)
                .fillMaxWidth()
                .testTag(CallTestTags.ENDED_BANNER),
            shape = MaterialTheme.shapes.large,
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
            ) {
                Text(
                    text = stringResource(R.string.call_ended),
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.weight(1f),
                )
                IconButton(onClick = onDismiss) {
                    Icon(
                        Icons.Filled.Close,
                        contentDescription = stringResource(R.string.call_action_dismiss),
                    )
                }
            }
        }
    }
}
