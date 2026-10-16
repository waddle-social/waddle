package social.waddle.android.feature.home

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.CloudOff
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.Sync
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.compositeOver
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay
import social.waddle.android.R
import social.waddle.android.client.ConnectionState
import social.waddle.android.theme.Waddle
import social.waddle.android.theme.WaddleColors

/** Which tint a connection notice wears (web connection banner tones). */
enum class BannerTone { MUTED, WARNING, DANGER, ACTION }

/** One connection notice: what to say, in which tone, with which glyph. */
data class BannerNotice(val tone: BannerTone, val title: Int, val body: Int?, val icon: ImageVector)

/**
 * The web's connection copy for each state. Connecting the first time
 * is routine and says only that; a dropped link says what still works;
 * an expired session asks for the user. `null`: nothing to say.
 */
fun bannerNoticeOf(state: ConnectionState, backOnline: Boolean): BannerNotice? = when (state) {
    ConnectionState.Connecting ->
        BannerNotice(BannerTone.MUTED, R.string.connection_banner_connecting, null, Icons.Outlined.Sync)
    is ConnectionState.Reconnecting -> BannerNotice(
        BannerTone.WARNING,
        R.string.connection_banner_reconnecting_title,
        R.string.connection_banner_reconnecting_body,
        Icons.Outlined.Sync,
    )
    ConnectionState.Offline -> BannerNotice(
        BannerTone.MUTED,
        R.string.connection_banner_offline_title,
        R.string.connection_banner_offline_body,
        Icons.Outlined.CloudOff,
    )
    ConnectionState.AuthFailed -> BannerNotice(
        BannerTone.DANGER,
        R.string.connection_banner_attention_title,
        R.string.connection_banner_attention_body,
        Icons.Outlined.ErrorOutline,
    )
    ConnectionState.Ready -> if (backOnline) {
        BannerNotice(BannerTone.ACTION, R.string.connection_banner_back, null, Icons.Outlined.CheckCircle)
    } else {
        null
    }
    ConnectionState.Idle, ConnectionState.Paused -> null
}

/**
 * The connection strip under the header: a tinted panel (10% of its
 * tone, a 1dp ring of it), never a glow and never mistaken for a field.
 * After a drop, "Back online" shows for 4.2 seconds once the link is
 * back. Announced politely to screen readers.
 */
@Composable
fun ConnectionBanner(
    state: ConnectionState,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var dropped by remember { mutableStateOf(false) }
    var backOnline by remember { mutableStateOf(false) }
    LaunchedEffect(state) {
        when (state) {
            is ConnectionState.Reconnecting, ConnectionState.Offline, ConnectionState.AuthFailed -> dropped = true
            ConnectionState.Ready -> if (dropped) {
                dropped = false
                backOnline = true
                delay(BACK_ONLINE_MILLIS)
                backOnline = false
            }
            else -> Unit
        }
    }
    val notice = bannerNoticeOf(state, backOnline)
    // Keep the last notice while the strip animates away (a plain
    // holder: remembering it must not trigger another composition).
    val last = remember { LastNotice() }
    if (notice != null) last.value = notice
    AnimatedVisibility(
        visible = notice != null,
        enter = expandVertically() + fadeIn(),
        exit = shrinkVertically() + fadeOut(),
        modifier = modifier,
    ) {
        last.value?.let { BannerPanel(it, showRetry = state is ConnectionState.Reconnecting, onRetry = onRetry) }
    }
}

@Composable
private fun BannerPanel(notice: BannerNotice, showRetry: Boolean, onRetry: () -> Unit) {
    val colors = Waddle.colors
    val (tone, ink) = toneColors(notice.tone, colors)
    val shape = RoundedCornerShape(Waddle.radius.panel)
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = Modifier
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.xs)
            .fillMaxWidth()
            .heightIn(min = Waddle.control.touch)
            .background(tone.copy(alpha = FILL_ALPHA).compositeOver(colors.bg), shape)
            .border(1.dp, tone.copy(alpha = RING_ALPHA), shape)
            .padding(start = Waddle.space.sm, end = Waddle.space.xxs)
            .semantics(mergeDescendants = true) { liveRegion = LiveRegionMode.Polite },
    ) {
        Icon(notice.icon, contentDescription = null, tint = ink, modifier = Modifier.size(ICON))
        Column(
            modifier = Modifier
                .weight(1f)
                .padding(vertical = Waddle.space.xs),
        ) {
            Text(text = stringResource(notice.title), style = MaterialTheme.typography.labelLarge, color = colors.fg)
            notice.body?.let {
                Text(text = stringResource(it), style = MaterialTheme.typography.bodySmall, color = colors.fgMuted)
            }
        }
        // Backoff can reach a minute; let the user skip the wait.
        if (showRetry) {
            TextButton(onClick = onRetry) { Text(text = stringResource(R.string.connection_retry)) }
        }
    }
}

/** The tone's fill source and its readable ink. */
private fun toneColors(tone: BannerTone, colors: WaddleColors): Pair<Color, Color> = when (tone) {
    BannerTone.MUTED -> colors.fgMuted to colors.fgMuted
    BannerTone.WARNING -> colors.warning to colors.warningText
    BannerTone.DANGER -> colors.danger to colors.dangerText
    BannerTone.ACTION -> colors.action to colors.actionText
}

private class LastNotice(var value: BannerNotice? = null)

private const val BACK_ONLINE_MILLIS = 4_200L
private const val FILL_ALPHA = 0.10f
private const val RING_ALPHA = 0.35f
private val ICON = 18.dp
