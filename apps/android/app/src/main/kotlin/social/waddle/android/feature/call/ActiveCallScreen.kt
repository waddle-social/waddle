package social.waddle.android.feature.call

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.VolumeUp
import androidx.compose.material.icons.outlined.CallEnd
import androidx.compose.material.icons.outlined.Cameraswitch
import androidx.compose.material.icons.outlined.FrontHand
import androidx.compose.material.icons.outlined.KeyboardArrowDown
import androidx.compose.material.icons.outlined.Mic
import androidx.compose.material.icons.outlined.MicOff
import androidx.compose.material.icons.outlined.Videocam
import androidx.compose.material.icons.outlined.VideocamOff
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import social.waddle.android.R
import social.waddle.android.client.calls.CallKind
import social.waddle.android.client.calls.CallState
import social.waddle.android.client.normalizedBareJid
import social.waddle.android.jid.bareJidOf
import social.waddle.android.jid.localpartOf
import social.waddle.android.theme.Waddle
import social.waddle.android.theme.WaddleTokens
import social.waddle.android.theme.WaddleType
import social.waddle.android.ui.huddle.AvatarSize
import social.waddle.android.ui.huddle.HuddleAvatar
import social.waddle.android.ui.huddle.HuddleTag
import social.waddle.android.ui.huddle.Kicker
import social.waddle.android.ui.huddle.SpeakingBars
import social.waddle.android.ui.huddle.TagTone

/** Where a call is, as the stage shows it. */
sealed interface CallPhase {
    /** Our propose is out; no device has rung yet. */
    data object Calling : CallPhase

    /** A device of the callee is ringing. */
    data object Ringing : CallPhase

    /** Signalling done or in flight; media not flowing yet. */
    data object Connecting : CallPhase

    /** Media flowing for [seconds]. */
    data class Live(val seconds: Long) : CallPhase

    /** The media transport dropped and LiveKit is retrying. */
    data class Reconnecting(val seconds: Long) : CallPhase
}

/** The stage's phase from the call slot plus the media transport. */
fun callPhaseOf(state: CallState, durationSeconds: Long, media: CallMediaConnection): CallPhase? = when (state) {
    is CallState.Outgoing -> if (state.ringing) CallPhase.Ringing else CallPhase.Calling
    is CallState.MucPending -> CallPhase.Connecting
    is CallState.Active -> when (media) {
        CallMediaConnection.Connected -> CallPhase.Live(durationSeconds)
        CallMediaConnection.Reconnecting, is CallMediaConnection.Failed -> CallPhase.Reconnecting(durationSeconds)
        CallMediaConnection.Idle, CallMediaConnection.Connecting -> CallPhase.Connecting
    }
    else -> null
}

/** Everything the in-call stage draws, SDK- and ViewModel-free. */
@Immutable
data class ActiveCallUi(
    /** The person (DM) or room (group) this call is with. */
    val title: String,
    /** Real bare JID behind a DM's avatar; `null` = initials. */
    val avatarJid: String?,
    val group: Boolean,
    val video: Boolean,
    val phase: CallPhase,
    /** DM only: LiveKit hears the other person. */
    val peerSpeaking: Boolean = false,
    /** Group only: one tile per participant. */
    val roster: List<MucRosterEntry> = emptyList(),
    val micEnabled: Boolean = true,
    val cameraEnabled: Boolean = false,
    val speakerOn: Boolean = false,
    /** Our raised hand; `null` hides the control (DM calls, group setup). */
    val handRaised: Boolean? = null,
)

/** What the stage's controls do. */
@Immutable
data class ActiveCallActions(
    val onMinimize: () -> Unit = {},
    val onToggleMic: () -> Unit = {},
    val onToggleCamera: () -> Unit = {},
    val onFlipCamera: () -> Unit = {},
    val onToggleSpeaker: () -> Unit = {},
    val onToggleHandRaised: () -> Unit = {},
    val onHangUp: () -> Unit = {},
)

/**
 * The in-call surface for outgoing-ring, group-call-setup, and active
 * phases: always the night stage. Audio calls put the person (or the
 * room's participants) at the centre; video calls put the remote track
 * full-bleed with the local preview picture-in-picture.
 */
@Composable
fun ActiveCallScreen(
    state: CallState,
    viewModel: CallViewModel,
    onMinimize: () -> Unit,
) {
    val (peerJid, isVideoCall) = when (state) {
        is CallState.Outgoing -> state.to to state.media.video
        is CallState.Active -> state.peer to state.media.video
        is CallState.MucPending -> state.roomJid to state.media.video
        else -> return
    }
    val isMucCall = state is CallState.MucPending ||
        (state is CallState.Active && state.kind == CallKind.MUC)
    val isActiveMucCall = state is CallState.Active && state.kind == CallKind.MUC
    // System back must not pop the hidden nav stack under the call
    // surface — collapse to the banner instead.
    BackHandler(onBack = onMinimize)

    val durationSeconds by viewModel.durationSeconds.collectAsStateWithLifecycle()
    val connection by viewModel.media.connection.collectAsStateWithLifecycle()
    val remoteVideo by viewModel.media.remoteVideo.collectAsStateWithLifecycle()
    val localVideo by viewModel.media.localVideo.collectAsStateWithLifecycle()
    val micEnabled by viewModel.media.micEnabled.collectAsStateWithLifecycle()
    val cameraEnabled by viewModel.media.cameraEnabled.collectAsStateWithLifecycle()
    val speakerOn by viewModel.media.speakerOn.collectAsStateWithLifecycle()
    val mucRoster by viewModel.mucRoster.collectAsStateWithLifecycle()
    val selfHandRaised by viewModel.selfHandRaised.collectAsStateWithLifecycle()
    val speaking by viewModel.speaking.collectAsStateWithLifecycle()
    val phase = callPhaseOf(state, durationSeconds, connection) ?: return

    ActiveCallContent(
        ui = ActiveCallUi(
            title = localpartOf(bareJidOf(peerJid)),
            // A group call's stage is the room, not a person.
            avatarJid = bareJidOf(peerJid).takeUnless { isMucCall },
            group = isMucCall,
            video = isVideoCall,
            phase = phase,
            peerSpeaking = !isMucCall && normalizedBareJid(peerJid) in speaking,
            roster = mucRoster,
            micEnabled = micEnabled,
            cameraEnabled = cameraEnabled,
            speakerOn = speakerOn,
            // Raise-hand is a Muji presence re-stamp — only live once
            // the group call is Active on the wire.
            handRaised = selfHandRaised.takeIf { isActiveMucCall },
        ),
        actions = ActiveCallActions(
            onMinimize = onMinimize,
            onToggleMic = viewModel::toggleMic,
            onToggleCamera = viewModel::toggleCamera,
            onFlipCamera = viewModel::flipCamera,
            onToggleSpeaker = viewModel::toggleSpeaker,
            onToggleHandRaised = viewModel::toggleHandRaised,
            onHangUp = viewModel::hangUp,
        ),
        remoteVideo = remoteVideo,
        localVideo = localVideo,
    )
}

/** The night stage itself; stateless so screenshots and previews drive it. */
@Composable
fun ActiveCallContent(
    ui: ActiveCallUi,
    actions: ActiveCallActions,
    modifier: Modifier = Modifier,
    remoteVideo: VideoTrackHandle? = null,
    localVideo: VideoTrackHandle? = null,
) {
    NightStage {
        val colors = Waddle.colors
        val fullBleed = remoteVideo?.takeIf { ui.video }
        val fullBleedVideo = fullBleed != null
        Box(
            modifier = modifier
                .fillMaxSize()
                .background(colors.nightGround)
                .testTag(CallTestTags.ACTIVE_SCREEN),
        ) {
            if (fullBleed != null) {
                CallVideoView(handle = fullBleed, modifier = Modifier.fillMaxSize())
                VideoScrims()
            }
            Column(
                modifier = Modifier
                    .fillMaxSize()
                    .windowInsetsPadding(WindowInsets.safeDrawing),
            ) {
                StageTopRow(ui = ui, onMinimize = actions.onMinimize)
                Box(
                    modifier = Modifier
                        .weight(1f)
                        .fillMaxWidth(),
                ) {
                    when {
                        fullBleedVideo -> VideoCaption(ui, Modifier.align(Alignment.BottomCenter))
                        ui.group -> GroupStage(ui)
                        else -> PersonStage(ui)
                    }
                }
                CallControls(ui = ui, actions = actions)
            }
            if (ui.video && localVideo != null) {
                LocalPreview(
                    handle = localVideo,
                    onFlipCamera = actions.onFlipCamera,
                    modifier = Modifier
                        .align(Alignment.TopEnd)
                        .windowInsetsPadding(WindowInsets.safeDrawing)
                        .padding(top = TOP_ROW_HEIGHT, end = Waddle.space.md),
                )
            }
        }
    }
}

/** Minimize on the left, the live kicker centred, balanced on the right. */
@Composable
private fun StageTopRow(ui: ActiveCallUi, onMinimize: () -> Unit) {
    Box(
        modifier = Modifier
            .fillMaxWidth()
            .height(TOP_ROW_HEIGHT)
            .padding(horizontal = Waddle.space.xxs),
    ) {
        IconButton(onClick = onMinimize, modifier = Modifier.align(Alignment.CenterStart)) {
            Icon(
                Icons.Outlined.KeyboardArrowDown,
                contentDescription = stringResource(R.string.call_action_minimize),
                tint = Waddle.colors.fg,
            )
        }
        Kicker(
            text = stageKicker(ui),
            live = ui.phase is CallPhase.Live,
            modifier = Modifier.align(Alignment.Center),
        )
    }
}

@Composable
private fun stageKicker(ui: ActiveCallUi): String = stringResource(
    when {
        ui.phase !is CallPhase.Live -> when {
            ui.group && ui.video -> R.string.call_kicker_video_huddle
            ui.group -> R.string.call_kicker_voice_huddle
            ui.video -> R.string.call_kicker_video_call
            else -> R.string.call_kicker_voice_call
        }
        ui.group && ui.video -> R.string.call_kicker_live_video_huddle
        ui.group -> R.string.call_kicker_live_voice_huddle
        ui.video -> R.string.call_kicker_live_video_call
        else -> R.string.call_kicker_live_voice_call
    },
)

/** The mono line under the title: the running clock, or what is happening. */
@Composable
private fun stageStatus(phase: CallPhase): String = when (phase) {
    CallPhase.Calling -> stringResource(R.string.call_status_calling)
    CallPhase.Ringing -> stringResource(R.string.call_status_ringing)
    CallPhase.Connecting -> stringResource(R.string.call_status_connecting)
    is CallPhase.Live -> formatCallDuration(phase.seconds)
    is CallPhase.Reconnecting -> stringResource(R.string.call_stage_reconnecting, formatCallDuration(phase.seconds))
}

/** One person, centred: a DM call (or an outgoing ring). */
@Composable
private fun PersonStage(ui: ActiveCallUi) {
    BoxWithConstraints(modifier = Modifier.fillMaxSize()) {
        // Landscape and split screen: a smaller stage keeps the controls on screen.
        val avatar = if (maxHeight < COMPACT_STAGE_HEIGHT) STAGE_AVATAR_COMPACT else STAGE_AVATAR
        Column(
            modifier = Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = Waddle.space.lg),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center,
        ) {
            StageAvatar(
                jid = ui.avatarJid,
                name = ui.title,
                size = avatar,
                ring = when (ui.phase) {
                    CallPhase.Calling, CallPhase.Ringing -> StageRing.CALLING
                    CallPhase.Connecting -> StageRing.NONE
                    is CallPhase.Live, is CallPhase.Reconnecting ->
                        if (ui.peerSpeaking) StageRing.SPEAKING else StageRing.HUDDLE
                },
                modifier = Modifier.clearAndSetSemantics { },
            )
            // Room for the calling waves to spread without touching the name.
            Spacer(Modifier.height(Waddle.space.xl))
            Text(
                text = ui.title,
                style = Waddle.type.headline,
                color = Waddle.colors.fg,
                textAlign = TextAlign.Center,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.semantics { heading() },
            )
            Spacer(Modifier.height(Waddle.space.xs))
            StageClock(ui.phase)
            SpeakingLine(speaking = ui.peerSpeaking)
        }
    }
}

/** The room and everyone in it: a group call. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun GroupStage(ui: ActiveCallUi) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = Waddle.space.md),
        horizontalAlignment = Alignment.CenterHorizontally,
        // Few people sit mid-stage; a crowd scrolls from the top.
        verticalArrangement = Arrangement.Center,
    ) {
        Spacer(Modifier.height(Waddle.space.md))
        Text(
            text = ui.title,
            style = Waddle.type.headline,
            color = Waddle.colors.fg,
            textAlign = TextAlign.Center,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.semantics { heading() },
        )
        Spacer(Modifier.height(Waddle.space.xs))
        StageClock(
            phase = ui.phase,
            count = ui.roster.size.takeIf { it > 0 && ui.phase is CallPhase.Live },
        )
        Spacer(Modifier.height(Waddle.space.xl))
        BoxWithConstraints(
            modifier = Modifier
                .fillMaxWidth()
                .testTag(CallTestTags.MUC_ROSTER),
        ) {
            val columns = if (ui.roster.size <= 2) 2 else GRID_COLUMNS
            val tileWidth = (maxWidth - TILE_GAP * (columns - 1)) / columns
            FlowRow(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(TILE_GAP, Alignment.CenterHorizontally),
                verticalArrangement = Arrangement.spacedBy(Waddle.space.lg),
                maxItemsInEachRow = columns,
            ) {
                ui.roster.forEach { entry ->
                    ParticipantTile(entry = entry, width = tileWidth)
                }
            }
        }
        Spacer(Modifier.height(Waddle.space.lg))
    }
}

/** Mono clock (or status) under the stage title, with the head count for groups. */
@Composable
private fun StageClock(phase: CallPhase, count: Int? = null) {
    val status = stageStatus(phase)
    val text = if (count != null) stringResource(R.string.call_stage_clock_count, status, count) else status
    Text(
        text = text,
        style = STAGE_CLOCK,
        color = if (phase is CallPhase.Reconnecting) Waddle.colors.warningText else Waddle.colors.fgMuted,
        textAlign = TextAlign.Center,
    )
}

/** Bars and "Speaking" under a DM's name; the row is always reserved so nothing jumps. */
@Composable
private fun SpeakingLine(speaking: Boolean) {
    Row(
        modifier = Modifier
            .padding(top = Waddle.space.md)
            .height(Waddle.space.md),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
    ) {
        if (speaking) {
            SpeakingBars()
            HuddleTag(text = stringResource(R.string.call_speaking), tone = TagTone.LIVE)
        }
    }
}

/**
 * One participant: the avatar with the huddle ring (ember and glowing
 * while they speak), a gold hand when raised, a muted mark, the name in
 * Space Grotesk, and speaking bars under it. Read as one item: "bob,
 * speaking, hand raised, muted".
 */
@Composable
private fun ParticipantTile(entry: MucRosterEntry, width: Dp) {
    val colors = Waddle.colors
    val states = listOfNotNull(
        stringResource(R.string.call_you).takeIf { entry.self },
        stringResource(R.string.call_speaking).takeIf { entry.speaking },
        stringResource(R.string.call_roster_hand_raised).takeIf { entry.handRaised },
        stringResource(R.string.call_roster_muted).takeIf { entry.muted },
    ).joinToString(", ")
    Column(
        modifier = Modifier
            .width(width)
            .semantics(mergeDescendants = true) {
                if (states.isNotEmpty()) stateDescription = states
            },
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Box(modifier = Modifier.clearAndSetSemantics { }) {
            HuddleAvatar(
                jid = entry.jid,
                name = entry.nick,
                size = AvatarSize.XL,
                inHuddle = true,
                speaking = entry.speaking,
                ground = colors.nightGround,
            )
            if (entry.handRaised) {
                TileBadge(
                    icon = Icons.Outlined.FrontHand,
                    fill = colors.gold,
                    ink = colors.nightGround,
                    modifier = Modifier
                        .align(Alignment.TopEnd)
                        .offset(x = BADGE_OUTSET, y = -BADGE_OUTSET),
                )
            }
            if (entry.muted) {
                TileBadge(
                    icon = Icons.Outlined.MicOff,
                    fill = colors.surface2,
                    ink = colors.fg,
                    modifier = Modifier
                        .align(Alignment.BottomEnd)
                        .offset(x = BADGE_OUTSET, y = BADGE_OUTSET),
                )
            }
        }
        Spacer(Modifier.height(Waddle.space.sm))
        Text(
            text = entry.nick,
            style = MaterialTheme.typography.titleSmall,
            color = colors.fg,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
        Box(
            modifier = Modifier
                .padding(top = Waddle.space.xxs)
                .height(Waddle.space.md),
            contentAlignment = Alignment.Center,
        ) {
            when {
                entry.speaking -> SpeakingBars()
                entry.self -> HuddleTag(text = stringResource(R.string.call_you))
            }
        }
    }
}

/** A small round mark on a tile's avatar, cut out of the ground. */
@Composable
private fun TileBadge(
    icon: ImageVector,
    fill: Color,
    ink: Color,
    modifier: Modifier = Modifier,
) {
    Box(
        modifier = modifier
            .size(Waddle.space.lg)
            .background(fill, CircleShape)
            .border(BADGE_BORDER, Waddle.colors.nightGround, CircleShape),
        contentAlignment = Alignment.Center,
    ) {
        Icon(icon, contentDescription = null, tint = ink, modifier = Modifier.size(BADGE_ICON))
    }
}

/** Over full-bleed video: who it is and the clock, low on the frame. */
@Composable
private fun VideoCaption(ui: ActiveCallUi, modifier: Modifier = Modifier) {
    Column(
        modifier = modifier.padding(bottom = Waddle.space.md),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
        ) {
            Text(
                text = ui.title,
                style = Waddle.type.title,
                color = Waddle.colors.fg,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            if (ui.peerSpeaking) SpeakingBars()
        }
        StageClock(ui.phase)
    }
}

/** Night fades at the top and bottom so controls stay legible over any video. */
@Composable
private fun VideoScrims() {
    val ground = Waddle.colors.nightGround
    Box(modifier = Modifier.fillMaxSize()) {
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .height(SCRIM_TOP)
                .align(Alignment.TopCenter)
                .background(Brush.verticalGradient(listOf(ground.copy(alpha = SCRIM_ALPHA), ground.copy(alpha = 0f)))),
        )
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .height(SCRIM_BOTTOM)
                .align(Alignment.BottomCenter)
                .background(Brush.verticalGradient(listOf(ground.copy(alpha = 0f), ground.copy(alpha = SCRIM_ALPHA)))),
        )
    }
}

/** Our own camera, picture-in-picture, with the flip control on it. */
@Composable
private fun LocalPreview(handle: VideoTrackHandle, onFlipCamera: () -> Unit, modifier: Modifier = Modifier) {
    val shape = RoundedCornerShape(Waddle.radius.panel)
    Box(
        modifier = modifier
            .width(LOCAL_PREVIEW_WIDTH)
            .aspectRatio(LOCAL_PREVIEW_ASPECT)
            .clip(shape)
            .border(1.dp, Waddle.colors.border, shape),
    ) {
        CallVideoView(handle = handle, modifier = Modifier.fillMaxSize())
        IconButton(onClick = onFlipCamera, modifier = Modifier.align(Alignment.BottomEnd)) {
            Box(
                modifier = Modifier
                    .size(Waddle.control.sm)
                    .background(Waddle.colors.nightGround.copy(alpha = SCRIM_ALPHA), CircleShape),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    Icons.Outlined.Cameraswitch,
                    contentDescription = stringResource(R.string.call_action_flip_camera),
                    tint = Waddle.colors.fg,
                    modifier = Modifier.size(BADGE_ICON + Waddle.space.xxs),
                )
            }
        }
    }
}

/**
 * The control row: mute, raise hand (groups), camera (video), speaker,
 * and leave. Each is a labelled switch; leave is the only red.
 */
@Composable
private fun CallControls(ui: ActiveCallUi, actions: ActiveCallActions) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Waddle.space.xs)
            .padding(top = Waddle.space.md, bottom = Waddle.space.lg),
        horizontalArrangement = Arrangement.SpaceEvenly,
        verticalAlignment = Alignment.Top,
    ) {
        val control = Modifier.weight(1f)
        CallControl(
            icon = if (ui.micEnabled) Icons.Outlined.Mic else Icons.Outlined.MicOff,
            label = stringResource(R.string.call_control_mute),
            onClick = actions.onToggleMic,
            engaged = !ui.micEnabled,
            modifier = control.testTag(CallTestTags.MUTE_BUTTON),
        )
        if (ui.handRaised != null) {
            CallControl(
                icon = Icons.Outlined.FrontHand,
                label = stringResource(R.string.call_action_raise_hand),
                onClick = actions.onToggleHandRaised,
                tone = ControlTone.GOLD,
                engaged = ui.handRaised,
                modifier = control.testTag(CallTestTags.RAISE_HAND_BUTTON),
            )
        }
        if (ui.video) {
            CallControl(
                icon = if (ui.cameraEnabled) Icons.Outlined.Videocam else Icons.Outlined.VideocamOff,
                label = stringResource(R.string.call_control_camera_off),
                onClick = actions.onToggleCamera,
                engaged = !ui.cameraEnabled,
                modifier = control,
            )
        }
        CallControl(
            icon = Icons.AutoMirrored.Outlined.VolumeUp,
            label = stringResource(R.string.call_control_speaker),
            onClick = actions.onToggleSpeaker,
            engaged = ui.speakerOn,
            modifier = control,
        )
        CallControl(
            icon = Icons.Outlined.CallEnd,
            label = stringResource(if (ui.group) R.string.call_control_leave else R.string.call_control_end),
            onClick = actions.onHangUp,
            tone = ControlTone.DANGER,
            modifier = control.testTag(CallTestTags.HANG_UP_BUTTON),
        )
    }
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

/** The stage clock: mono at message size, steady digits. */
private val STAGE_CLOCK = WaddleType.time.copy(fontSize = WaddleTokens.Huddle.FontSize.message)

private val TOP_ROW_HEIGHT = CALL_CONTROL
private val STAGE_AVATAR = WaddleTokens.Space.xxxl * 2
private val STAGE_AVATAR_COMPACT = 88.dp
private val COMPACT_STAGE_HEIGHT = 420.dp
private const val GRID_COLUMNS = 3
private val TILE_GAP = WaddleTokens.Space.sm
private val BADGE_OUTSET = WaddleTokens.Space.xxs
private val BADGE_BORDER = 2.dp
private val BADGE_ICON = 14.dp
private val SCRIM_TOP = 160.dp
private val SCRIM_BOTTOM = 240.dp
private const val SCRIM_ALPHA = 0.72f
private val LOCAL_PREVIEW_WIDTH = 104.dp
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
    const val MUTE_BUTTON = "call-mute"
    const val BANNER = "call-banner"
    const val ENDED_BANNER = "call-ended-banner"
    const val FAKE_VIDEO_SURFACE = "call-fake-video"
    const val MUC_ROSTER = "call-muc-roster"
    const val RAISE_HAND_BUTTON = "call-raise-hand"
}
