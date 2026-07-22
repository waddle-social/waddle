package social.waddle.android.feature.conversation

import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Call
import androidx.compose.material.icons.outlined.CallEnd
import androidx.compose.material.icons.outlined.Videocam
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import social.waddle.android.R
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.feature.call.formatCallDuration
import social.waddle.android.jid.bareJidOf
import social.waddle.android.jid.localpartOf
import java.time.Duration
import java.time.format.DateTimeParseException

/**
 * Compact feed row for `urn:waddle:call-thread:0` anchors: a
 * "started a call" line for the anchor and a "call ended · duration"
 * line for the XEP-0422 ended marker, replacing the empty bubble the
 * server-enriched stanzas would otherwise render as.
 */
@Composable
internal fun CallTimelineRow(item: TimelineItem) {
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
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 6.dp),
    ) {
        Icon(
            icon,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.size(18.dp),
        )
        Text(
            text = text,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(start = 8.dp),
        )
    }
}

/** `PT5M30S` → `5:30`; unparseable input falls back to the raw text. */
private fun formatIsoDuration(iso: String): String = try {
    formatCallDuration(Duration.parse(iso).seconds)
} catch (_: DateTimeParseException) {
    iso
}
