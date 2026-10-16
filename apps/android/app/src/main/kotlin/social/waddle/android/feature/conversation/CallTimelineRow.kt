package social.waddle.android.feature.conversation

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Call
import androidx.compose.material.icons.outlined.CallEnd
import androidx.compose.material.icons.outlined.Videocam
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import social.waddle.android.R
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.feature.call.formatCallDuration
import social.waddle.android.jid.bareJidOf
import social.waddle.android.jid.localpartOf
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.huddle.AvatarSize
import java.time.Duration
import java.time.Instant
import java.time.format.DateTimeParseException

/**
 * Compact feed row for `urn:waddle:call-thread:0` anchors: a "started a
 * call" line for the anchor and a "call ended · duration" line for the
 * XEP-0422 ended marker, replacing the empty bubble the server-enriched
 * stanzas would otherwise render as. It sits on the message grid: the
 * glyph in the avatar column, the line and its time in the text column.
 * History, not a live thing, so it stays quiet (no ember).
 */
@Composable
internal fun CallTimelineRow(item: TimelineItem, sentAt: Instant) {
    val anchor = item.callAnchor
    val ended = item.callEndedMarker
    val (icon, text) = when {
        anchor != null -> {
            val video = "video" in anchor.media
            val initiator = localpartOf(bareJidOf(anchor.initiator))
            val label = stringResource(
                if (video) R.string.call_row_video_started else R.string.call_row_started,
                initiator,
            )
            (if (video) Icons.Outlined.Videocam else Icons.Outlined.Call) to label
        }
        ended != null -> Icons.Outlined.CallEnd to stringResource(
            R.string.call_row_ended,
            formatIsoDuration(ended.duration),
        )
        else -> return
    }
    val colors = Waddle.colors
    val clock = rememberConversationClock()
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = Modifier
            .fillMaxWidth()
            .semantics(mergeDescendants = true) {}
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.xs),
    ) {
        Box(modifier = Modifier.width(AvatarSize.MESSAGE.dp), contentAlignment = Alignment.Center) {
            Box(
                contentAlignment = Alignment.Center,
                modifier = Modifier
                    .size(AvatarSize.SM.dp)
                    .background(colors.surface2, CircleShape),
            ) {
                Icon(icon, contentDescription = null, tint = colors.fgMuted, modifier = Modifier.size(CALL_ICON))
            }
        }
        Text(
            text = text,
            style = MetaStyle,
            color = colors.fgMuted,
            modifier = Modifier.weight(1f, fill = false),
        )
        Text(text = clock.time(sentAt), style = Waddle.type.time, color = colors.fgMuted, maxLines = 1)
    }
}

/** `PT5M30S` → `5:30`; unparseable input falls back to the raw text. */
private fun formatIsoDuration(iso: String): String = try {
    formatCallDuration(Duration.parse(iso).seconds)
} catch (_: DateTimeParseException) {
    iso
}

private val CALL_ICON = 16.dp
