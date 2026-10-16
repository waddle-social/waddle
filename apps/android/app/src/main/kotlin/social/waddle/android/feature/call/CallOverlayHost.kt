package social.waddle.android.feature.call

import androidx.annotation.StringRes
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.displayCutout
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.layout.union
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.CallEnd
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.coroutines.flow.StateFlow
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.client.calls.CallEndReason
import social.waddle.android.client.calls.CallKind
import social.waddle.android.client.calls.CallState
import social.waddle.android.client.prefs.ThemeMode
import social.waddle.android.jid.bareJidOf
import social.waddle.android.jid.localpartOf
import social.waddle.android.theme.Waddle
import social.waddle.android.theme.WaddleTheme
import social.waddle.android.ui.huddle.LiveDot
import social.waddle.client.ffi.WaddleJingleReason

/**
 * Global call surface router around the navigation shell: `Incoming`
 * renders the full-screen ring, `Outgoing`/`Active` the in-call screen
 * (or the tap-to-return banner when minimized), and `Ended` a
 * dismissable banner. Driven entirely by the session manager's call
 * slot, so it works no matter which screen is open.
 *
 * Banners take their own row above [content] instead of floating over
 * it: while one shows, it owns the status-bar inset and [content]'s top
 * bars sit right under it, so a back arrow or a title is never covered.
 * Without a banner [content] gets the status-bar inset as usual.
 */
@Composable
fun CallOverlayHost(
    pendingCallAnswer: StateFlow<Boolean>,
    onCallAnswerConsumed: () -> Unit,
    content: @Composable () -> Unit = {},
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

    val call = state
    val inCall = call is CallState.Outgoing || call is CallState.Active || call is CallState.MucPending
    val banner = when {
        inCall && minimized -> CallBanner.Ongoing
        call is CallState.Ended -> CallBanner.Ended
        else -> null
    }

    Box(modifier = Modifier.fillMaxSize()) {
        Column(modifier = Modifier.fillMaxSize()) {
            when (banner) {
                CallBanner.Ongoing -> {
                    val seconds by viewModel.durationSeconds.collectAsStateWithLifecycle()
                    val connection by viewModel.media.connection.collectAsStateWithLifecycle()
                    OngoingCallBanner(
                        title = localpartOf(bareJidOf(callPeerOf(call).orEmpty())),
                        group = isGroupCall(call),
                        phase = callPhaseOf(call, seconds, connection) ?: CallPhase.Connecting,
                        onExpand = { viewModel.setMinimized(false) },
                        onHangUp = viewModel::hangUp,
                    )
                }
                CallBanner.Ended -> CallEndedBanner(
                    reason = (call as? CallState.Ended)?.reason,
                    onDismiss = viewModel::dismissEnded,
                )
                null -> Unit
            }
            Box(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxWidth()
                    .then(if (banner != null) Modifier.consumeWindowInsets(BannerInsets.top) else Modifier),
            ) {
                content()
            }
        }

        when (call) {
            is CallState.Incoming -> IncomingCallScreen(
                state = call,
                autoAnswer = autoAnswer,
                onAutoAnswerConsumed = onCallAnswerConsumed,
                onAccept = viewModel::accept,
                onDecline = viewModel::decline,
            )
            // MucPending renders the same in-call surface as an outgoing
            // ring: the XEP-0272 setup runs multi-second waits and the user
            // needs the hang-up affordance to abandon it.
            is CallState.Outgoing, is CallState.Active, is CallState.MucPending ->
                if (!minimized) {
                    ActiveCallScreen(
                        state = call,
                        viewModel = viewModel,
                        onMinimize = { viewModel.setMinimized(true) },
                    )
                }
            is CallState.Ended, CallState.Idle -> Unit
        }
    }
}

private enum class CallBanner { Ongoing, Ended }

private fun callPeerOf(state: CallState): String? = when (state) {
    is CallState.Outgoing -> state.to
    is CallState.Active -> state.peer
    is CallState.MucPending -> state.roomJid
    else -> null
}

private fun isGroupCall(state: CallState): Boolean =
    state is CallState.MucPending || (state is CallState.Active && state.kind == CallKind.MUC)

/** The insets a top banner pads for, and hands over as consumed to the content below it. */
private object BannerInsets {
    val top: WindowInsets
        @Composable get() = WindowInsets.statusBars.union(WindowInsets.displayCutout).only(WindowInsetsSides.Top)

    val banner: WindowInsets
        @Composable get() = WindowInsets.statusBars.union(WindowInsets.displayCutout)
            .only(WindowInsetsSides.Top + WindowInsetsSides.Horizontal)
}

/**
 * The minimized call, as a strip across the top that stays night in
 * either theme (toasts stay night): live dot, who, the running clock in
 * ember mono, and a red end control. The strip returns to the call.
 */
@Composable
internal fun OngoingCallBanner(
    title: String,
    group: Boolean,
    phase: CallPhase,
    onExpand: () -> Unit,
    onHangUp: () -> Unit,
) {
    LightSystemBarIcons(statusBar = true, navigationBar = false)
    WaddleTheme(themeMode = ThemeMode.NIGHT) {
        val colors = Waddle.colors
        val returnLabel = stringResource(R.string.call_banner_return)
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .background(colors.nightGround)
                .hairline(colors.live)
                .clickable(onClickLabel = returnLabel, onClick = onExpand)
                .windowInsetsPadding(BannerInsets.banner)
                .heightIn(min = CALL_CONTROL)
                .padding(start = Waddle.space.md, end = Waddle.space.xxs)
                .testTag(CallTestTags.BANNER),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        ) {
            LiveDot()
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = title,
                    style = MaterialTheme.typography.titleSmall,
                    color = colors.fg,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    text = returnLabel,
                    style = MaterialTheme.typography.bodySmall,
                    color = colors.fgMuted,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Text(
                text = when (phase) {
                    is CallPhase.Live -> formatCallDuration(phase.seconds)
                    is CallPhase.Reconnecting -> stringResource(R.string.call_kicker_reconnecting)
                    CallPhase.Ringing -> stringResource(R.string.call_status_ringing)
                    CallPhase.Calling -> stringResource(R.string.call_status_calling)
                    CallPhase.Connecting -> stringResource(R.string.call_status_connecting)
                },
                style = Waddle.type.time,
                color = when (phase) {
                    is CallPhase.Live -> colors.liveText
                    is CallPhase.Reconnecting -> colors.warningText
                    else -> colors.fgMuted
                },
                maxLines = 1,
            )
            IconButton(onClick = onHangUp) {
                Box(
                    modifier = Modifier
                        .size(Waddle.control.sm)
                        .background(colors.danger, CircleShape),
                    contentAlignment = Alignment.Center,
                ) {
                    Icon(
                        Icons.Outlined.CallEnd,
                        contentDescription = stringResource(
                            if (group) R.string.call_control_leave else R.string.call_control_end,
                        ),
                        tint = colors.dangerFg,
                        modifier = Modifier.size(Waddle.space.md + Waddle.space.xxs),
                    )
                }
            }
        }
    }
}

/**
 * The call is over: a night strip naming why (declined, no answer, the
 * connection dropped…) until dismissed. Not ember: nothing is live.
 * Announced politely to screen readers.
 */
@Composable
internal fun CallEndedBanner(reason: CallEndReason?, onDismiss: () -> Unit) {
    LightSystemBarIcons(statusBar = true, navigationBar = false)
    WaddleTheme(themeMode = ThemeMode.NIGHT) {
        val colors = Waddle.colors
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .background(colors.surface)
                .hairline(colors.border)
                .windowInsetsPadding(BannerInsets.banner)
                .heightIn(min = CALL_CONTROL)
                .padding(start = Waddle.space.md, end = Waddle.space.xxs)
                .semantics(mergeDescendants = true) { liveRegion = LiveRegionMode.Polite }
                .testTag(CallTestTags.ENDED_BANNER),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        ) {
            Box(
                modifier = Modifier
                    .size(Waddle.control.sm)
                    .background(colors.surface2, CircleShape),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    Icons.Outlined.CallEnd,
                    contentDescription = null,
                    tint = colors.fgMuted,
                    modifier = Modifier.size(Waddle.space.md + Waddle.space.xxs),
                )
            }
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = stringResource(R.string.call_ended),
                    style = MaterialTheme.typography.titleSmall,
                    color = colors.fg,
                    maxLines = 1,
                )
                Text(
                    text = stringResource(callEndedReasonRes(reason)),
                    style = MaterialTheme.typography.bodySmall,
                    color = colors.fgMuted,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            IconButton(onClick = onDismiss) {
                Icon(
                    Icons.Outlined.Close,
                    contentDescription = stringResource(R.string.call_action_dismiss),
                    tint = colors.fgMuted,
                )
            }
        }
    }
}

/** Why a call ended, in words a person would use. */
@StringRes
fun callEndedReasonRes(reason: CallEndReason?): Int = when (reason) {
    CallEndReason.Rejected -> R.string.call_ended_declined
    CallEndReason.Retracted -> R.string.call_ended_cancelled
    CallEndReason.Expired -> R.string.call_ended_elsewhere
    CallEndReason.Timeout -> R.string.call_ended_no_answer
    CallEndReason.Error -> R.string.call_ended_failed
    is CallEndReason.Finished -> jingleEndReasonRes(reason.reason)
    null -> R.string.call_ended_hung_up
}

/** A XEP-0166 §7.4 terminate condition, in the same words. */
@StringRes
private fun jingleEndReasonRes(reason: WaddleJingleReason?): Int = when (reason) {
    null, WaddleJingleReason.SUCCESS -> R.string.call_ended_hung_up
    WaddleJingleReason.DECLINE -> R.string.call_ended_declined
    WaddleJingleReason.BUSY -> R.string.call_ended_busy
    WaddleJingleReason.CANCEL -> R.string.call_ended_cancelled
    WaddleJingleReason.TIMEOUT, WaddleJingleReason.EXPIRED -> R.string.call_ended_no_answer
    WaddleJingleReason.ALTERNATIVE_SESSION -> R.string.call_ended_elsewhere
    WaddleJingleReason.CONNECTIVITY_ERROR, WaddleJingleReason.FAILED_TRANSPORT,
    WaddleJingleReason.MEDIA_ERROR, WaddleJingleReason.GONE,
    -> R.string.call_ended_connection_lost
    WaddleJingleReason.FAILED_APPLICATION, WaddleJingleReason.GENERAL_ERROR,
    WaddleJingleReason.INCOMPATIBLE_PARAMETERS, WaddleJingleReason.SECURITY_ERROR,
    WaddleJingleReason.UNSUPPORTED_APPLICATIONS, WaddleJingleReason.UNSUPPORTED_TRANSPORTS,
    -> R.string.call_ended_failed
}

/** A 1dp rule along the bottom edge. */
private fun Modifier.hairline(color: Color): Modifier = drawBehind {
    val stroke = 1.dp.toPx()
    val y = size.height - stroke / 2f
    drawLine(color, Offset(0f, y), Offset(size.width, y), strokeWidth = stroke)
}
