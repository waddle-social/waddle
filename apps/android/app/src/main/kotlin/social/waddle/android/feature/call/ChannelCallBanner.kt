package social.waddle.android.feature.call

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Call
import androidx.compose.material.icons.outlined.CallEnd
import androidx.compose.material.icons.outlined.Videocam
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.text.style.TextOverflow
import social.waddle.android.R
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.huddle.AvatarSize
import social.waddle.android.ui.huddle.HuddleAvatar
import social.waddle.android.ui.huddle.HuddleCard
import social.waddle.android.ui.huddle.Kicker
import social.waddle.android.ui.huddle.PillButton
import social.waddle.android.ui.huddle.PillTone

/**
 * Above-timeline live-call surface for channels (web
 * ConversationCallBanner group branch, styled as the web's live call
 * card): who is in the huddle plus an ember Join while the room's call
 * runs without us, a slim live line while our own call in this room
 * sits minimized behind the chat, and the retained-call cleanup.
 */
@Composable
fun ChannelCallBanner(
    banner: ChannelCallBannerState,
    onJoin: () -> Unit,
    onLeaveRetained: () -> Unit = {},
) {
    when (banner) {
        ChannelCallBannerState.Hidden -> Unit
        is ChannelCallBannerState.Join -> JoinBanner(banner, onJoin)
        is ChannelCallBannerState.Ongoing -> OngoingLine(banner)
        is ChannelCallBannerState.LeaveRetained -> LeaveRetainedBanner(banner, onLeaveRetained)
    }
}

/** The live call card: ember border and glow, kicker, faces and names, Join. */
@Composable
private fun JoinBanner(banner: ChannelCallBannerState.Join, onJoin: () -> Unit) {
    HuddleCard(
        live = true,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.xs)
            .testTag(ChannelCallTestTags.BANNER),
        contentPadding = PaddingValues(
            start = Waddle.space.md,
            end = Waddle.space.md,
            top = Waddle.space.md,
            bottom = Waddle.space.sm,
        ),
    ) {
        Kicker(
            text = huddleKicker(video = banner.video, count = banner.participantCount),
            live = true,
        )
        Text(
            text = namesLine(banner.nicks, banner.participantCount),
            style = MaterialTheme.typography.titleSmall,
            color = Waddle.colors.fg,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.padding(top = Waddle.space.xs),
        )
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(top = Waddle.space.xxs),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            FaceStack(nicks = banner.nicks)
            Spacer(Modifier.weight(1f))
            PillButton(
                text = stringResource(if (banner.busy) R.string.call_join_busy else R.string.call_channel_join),
                onClick = onJoin,
                tone = if (banner.busy) PillTone.QUIET else PillTone.LIVE,
                icon = if (banner.busy) null else if (banner.video) Icons.Outlined.Videocam else Icons.Outlined.Call,
                enabled = !banner.busy,
                modifier = Modifier.testTag(ChannelCallTestTags.JOIN_BUTTON),
            )
        }
    }
}

/** Up to three faces, overlapping, cut out of the card. */
@Composable
private fun FaceStack(nicks: List<String>) {
    val shown = nicks.take(MAX_FACES)
    if (shown.isEmpty()) return
    val step = AvatarSize.XS.dp - FACE_OVERLAP
    Box(
        modifier = Modifier
            .width(AvatarSize.XS.dp + step * (shown.size - 1))
            .clearAndSetSemantics { },
    ) {
        shown.forEachIndexed { index, nick ->
            // Nicks only: no real JID here, so initials, never a guess.
            HuddleAvatar(
                jid = null,
                name = nick,
                size = AvatarSize.XS,
                ground = Waddle.colors.surface,
                modifier = Modifier.offset(x = step * index),
            )
        }
    }
}

/** While our own call in this room is minimized: one slim live line. */
@Composable
private fun OngoingLine(banner: ChannelCallBannerState.Ongoing) {
    Box(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = Waddle.control.md)
            .padding(horizontal = Waddle.space.md)
            .testTag(ChannelCallTestTags.ONGOING_PILL),
        contentAlignment = Alignment.CenterStart,
    ) {
        Kicker(
            text = stringResource(R.string.call_channel_ongoing_kicker, banner.participantCount),
            live = true,
        )
    }
}

/**
 * Cleanup surface for a call this resource still advertises but is no
 * longer in (web retained-call dock affordance): the leave runs the
 * presence-clear + cached-sid terminate without touching the slot.
 */
@Composable
private fun LeaveRetainedBanner(
    banner: ChannelCallBannerState.LeaveRetained,
    onLeaveRetained: () -> Unit,
) {
    HuddleCard(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.xs)
            .testTag(ChannelCallTestTags.RETAINED_BANNER),
        contentPadding = PaddingValues(
            start = Waddle.space.md,
            end = Waddle.space.sm,
            top = Waddle.space.sm,
            bottom = Waddle.space.sm,
        ),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        ) {
            Column(
                modifier = Modifier.weight(1f),
                verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
            ) {
                Kicker(text = huddleKicker(video = false, count = banner.participantCount))
                Text(
                    text = stringResource(R.string.call_channel_retained),
                    style = MaterialTheme.typography.bodySmall,
                    color = Waddle.colors.fgMuted,
                )
            }
            PillButton(
                text = stringResource(R.string.call_leave_retained),
                onClick = onLeaveRetained,
                tone = PillTone.DANGER,
                icon = Icons.Outlined.CallEnd,
                modifier = Modifier.testTag(ChannelCallTestTags.RETAINED_LEAVE_BUTTON),
            )
        }
    }
}

@Composable
private fun huddleKicker(video: Boolean, count: Int): String = stringResource(
    if (video) R.string.call_channel_kicker_video else R.string.call_channel_kicker_voice,
    count,
)

/** "mara", "mara and sol", "mara, sol and kai", "mara, sol and 3 others" (web call card copy). */
@Composable
private fun namesLine(nicks: List<String>, count: Int): String {
    return when {
        nicks.isEmpty() -> ""
        nicks.size == 1 && count <= 1 -> nicks[0]
        nicks.size == 2 && count <= 2 -> stringResource(R.string.call_names_two, nicks[0], nicks[1])
        nicks.size == 3 && count <= 3 -> stringResource(R.string.call_names_three, nicks[0], nicks[1], nicks[2])
        else -> {
            val others = maxOf(count, nicks.size) - 2
            pluralStringResource(R.plurals.call_names_more, others, nicks[0], nicks[1], others)
        }
    }
}

private const val MAX_FACES = 3
private val FACE_OVERLAP = Waddle.space.xs - Waddle.space.xxs / 2

/** Semantics tags shared with the instrumented group-call tests. */
object ChannelCallTestTags {
    const val BANNER = "channel-call-banner"
    const val JOIN_BUTTON = "channel-call-join"
    const val RETAINED_BANNER = "channel-call-retained-banner"
    const val RETAINED_LEAVE_BUTTON = "channel-call-retained-leave"
    const val ONGOING_PILL = "channel-call-ongoing"
    const val TOP_BAR_JOIN = "channel-call-top-bar-join"
    const val TOP_BAR_AUDIO = "channel-call-top-bar-audio"
    const val TOP_BAR_VIDEO = "channel-call-top-bar-video"
}
