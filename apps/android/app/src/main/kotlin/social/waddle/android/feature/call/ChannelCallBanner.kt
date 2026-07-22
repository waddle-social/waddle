package social.waddle.android.feature.call

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Call
import androidx.compose.material.icons.outlined.Videocam
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import social.waddle.android.R

/**
 * Above-timeline live-call surface for channels (web
 * ConversationCallBanner group branch): "N in call" + participant
 * nicks + Join while the room's call runs without us, and a compact
 * ongoing pill while our own call in this room sits minimized behind
 * the chat.
 */
@Composable
fun ChannelCallBanner(banner: ChannelCallBannerState, onJoin: () -> Unit) {
    when (banner) {
        ChannelCallBannerState.Hidden -> Unit
        is ChannelCallBannerState.Join -> JoinBanner(banner, onJoin)
        is ChannelCallBannerState.Ongoing -> OngoingPill(banner)
    }
}

@Composable
private fun JoinBanner(banner: ChannelCallBannerState.Join, onJoin: () -> Unit) {
    Surface(
        color = MaterialTheme.colorScheme.secondaryContainer,
        modifier = Modifier
            .fillMaxWidth()
            .testTag(ChannelCallTestTags.BANNER),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
        ) {
            Icon(
                if (banner.video) Icons.Outlined.Videocam else Icons.Outlined.Call,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onSecondaryContainer,
            )
            Column(
                modifier = Modifier
                    .weight(1f)
                    .padding(horizontal = 12.dp),
            ) {
                Text(
                    text = stringResource(R.string.call_in_call_count, banner.participantCount),
                    style = MaterialTheme.typography.titleSmall,
                    color = MaterialTheme.colorScheme.onSecondaryContainer,
                )
                Text(
                    text = banner.nicks.joinToString(),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSecondaryContainer,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Button(
                onClick = onJoin,
                enabled = !banner.busy,
                modifier = Modifier.testTag(ChannelCallTestTags.JOIN_BUTTON),
            ) {
                Text(
                    text = stringResource(
                        if (banner.busy) R.string.call_join_busy else R.string.call_action_join,
                    ),
                )
            }
        }
    }
}

/** Compact marker while our own call in this room is minimized. */
@Composable
private fun OngoingPill(banner: ChannelCallBannerState.Ongoing) {
    Surface(
        color = MaterialTheme.colorScheme.surfaceVariant,
        modifier = Modifier
            .fillMaxWidth()
            .testTag(ChannelCallTestTags.ONGOING_PILL),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 6.dp),
        ) {
            Icon(
                Icons.Outlined.Call,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.primary,
            )
            Text(
                text = stringResource(R.string.call_channel_ongoing, banner.participantCount),
                style = MaterialTheme.typography.labelLarge,
                modifier = Modifier.padding(start = 12.dp),
            )
        }
    }
}

/** Semantics tags shared with the instrumented group-call tests. */
object ChannelCallTestTags {
    const val BANNER = "channel-call-banner"
    const val JOIN_BUTTON = "channel-call-join"
    const val ONGOING_PILL = "channel-call-ongoing"
    const val TOP_BAR_JOIN = "channel-call-top-bar-join"
    const val TOP_BAR_AUDIO = "channel-call-top-bar-audio"
    const val TOP_BAR_VIDEO = "channel-call-top-bar-video"
}
