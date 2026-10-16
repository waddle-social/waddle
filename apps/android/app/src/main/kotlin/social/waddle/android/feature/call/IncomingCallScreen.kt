package social.waddle.android.feature.call

import android.Manifest
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Call
import androidx.compose.material.icons.outlined.CallEnd
import androidx.compose.material.icons.outlined.Videocam
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import social.waddle.android.R
import social.waddle.android.client.calls.CallState
import social.waddle.android.jid.bareJidOf
import social.waddle.android.jid.localpartOf
import social.waddle.android.theme.Waddle
import social.waddle.android.theme.WaddleTokens
import social.waddle.android.ui.huddle.Kicker

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

    IncomingCallContent(
        callerJid = bareJidOf(state.from),
        video = state.media.video,
        accepting = state.accepting,
        onAnswer = { permissionLauncher.launch(permissions) },
        onDecline = onDecline,
    )
}

/**
 * The ring itself, stateless: the caller big at the centre in an ember
 * ring that sends slow waves out (ringing is live), their address in
 * mono, and two large labelled targets at thumb height — Decline in
 * danger, Answer in teal.
 */
@Composable
fun IncomingCallContent(
    callerJid: String,
    video: Boolean,
    accepting: Boolean,
    onAnswer: () -> Unit,
    onDecline: () -> Unit,
    modifier: Modifier = Modifier,
) {
    NightStage {
        val colors = Waddle.colors
        val name = localpartOf(callerJid)
        Column(
            modifier = modifier
                .fillMaxSize()
                .background(colors.nightGround)
                .windowInsetsPadding(WindowInsets.safeDrawing)
                .testTag(CallTestTags.INCOMING_SCREEN),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Box(
                modifier = Modifier
                    .fillMaxWidth()
                    .height(CALL_CONTROL),
                contentAlignment = Alignment.Center,
            ) {
                Kicker(
                    text = stringResource(if (video) R.string.call_incoming_video else R.string.call_incoming_audio),
                    live = true,
                )
            }
            BoxWithConstraints(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxWidth(),
            ) {
                val avatar = if (maxHeight < COMPACT_RING_HEIGHT) RING_AVATAR_COMPACT else RING_AVATAR
                Column(
                    modifier = Modifier
                        .fillMaxSize()
                        .verticalScroll(rememberScrollState())
                        .padding(horizontal = Waddle.space.lg),
                    horizontalAlignment = Alignment.CenterHorizontally,
                    verticalArrangement = Arrangement.Center,
                ) {
                    StageAvatar(
                        jid = callerJid,
                        name = name,
                        size = avatar,
                        ring = StageRing.RINGING,
                        modifier = Modifier.clearAndSetSemantics { },
                    )
                    Spacer(Modifier.height(Waddle.space.xl))
                    Text(
                        text = name,
                        style = Waddle.type.display,
                        color = colors.fg,
                        textAlign = TextAlign.Center,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.semantics { heading() },
                    )
                    Spacer(Modifier.height(Waddle.space.xs))
                    Text(
                        text = callerJid,
                        style = Waddle.type.time,
                        color = colors.fgMuted,
                        textAlign = TextAlign.Center,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    ConnectingLine(visible = accepting)
                }
            }
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = Waddle.space.xxl)
                    .padding(top = Waddle.space.md, bottom = Waddle.space.xl),
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                CallControl(
                    icon = Icons.Outlined.CallEnd,
                    label = stringResource(R.string.call_action_decline),
                    onClick = onDecline,
                    tone = ControlTone.DANGER,
                    size = CALL_CONTROL_LARGE,
                    modifier = Modifier.testTag(CallTestTags.DECLINE_BUTTON),
                )
                CallControl(
                    icon = if (video) Icons.Outlined.Videocam else Icons.Outlined.Call,
                    label = stringResource(R.string.call_action_accept),
                    onClick = onAnswer,
                    tone = ControlTone.ACTION,
                    size = CALL_CONTROL_LARGE,
                    enabled = !accepting,
                    modifier = Modifier.testTag(CallTestTags.ACCEPT_BUTTON),
                )
            }
        }
    }
}

/** "Connecting…" under the caller once Answer is tapped; space reserved so nothing jumps. */
@Composable
private fun ConnectingLine(visible: Boolean) {
    Row(
        modifier = Modifier
            .padding(top = Waddle.space.lg)
            .height(Waddle.space.lg),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
    ) {
        if (visible) {
            CircularProgressIndicator(
                modifier = Modifier.size(Waddle.space.md),
                color = Waddle.colors.action,
                strokeWidth = PROGRESS_STROKE,
            )
            Text(
                text = stringResource(R.string.call_status_connecting),
                style = Waddle.type.control,
                color = Waddle.colors.fgMuted,
            )
        }
    }
}

private val RING_AVATAR = WaddleTokens.Space.xxxl * 2
private val RING_AVATAR_COMPACT = 88.dp
private val COMPACT_RING_HEIGHT = 420.dp
private val PROGRESS_STROKE = 2.dp
